use crate::{BoxFuture, Event, EventSource, RegistrationError, RoomId, SourceError};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};
use tokio::{sync::watch, time::Instant};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TimerId(u64);

/// One locally scheduled event for an explicitly authorized room.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Scheduled {
    pub timer: TimerId,
    pub room: RoomId,
}

struct Timer {
    room: RoomId,
    next: Instant,
    interval: Option<Duration>,
}

/// Bounded, monotonic timers owned by the existing event source.
pub struct Schedule {
    rooms: HashSet<RoomId>,
    timers: HashMap<TimerId, Timer>,
    last: u64,
}

impl Schedule {
    pub fn new(authorized_rooms: &[RoomId]) -> Result<Self, RegistrationError> {
        if !(1..=16).contains(&authorized_rooms.len()) {
            return Err(RegistrationError::Invalid);
        }
        let rooms: HashSet<_> = authorized_rooms.iter().copied().collect();
        if rooms.len() != authorized_rooms.len() {
            return Err(RegistrationError::Duplicate);
        }
        Ok(Self {
            rooms,
            timers: HashMap::new(),
            last: 0,
        })
    }

    /// Schedule one event or a repeating event, up to 128 active timers.
    pub fn add(
        &mut self,
        room: RoomId,
        delay: Duration,
        interval: Option<Duration>,
    ) -> Result<TimerId, RegistrationError> {
        let valid = |duration| {
            (Duration::from_millis(10)..=Duration::from_secs(7 * 86_400)).contains(&duration)
        };
        if !self.rooms.contains(&room)
            || !valid(delay)
            || interval.is_some_and(|interval| !valid(interval))
        {
            return Err(RegistrationError::Invalid);
        }
        if self.timers.len() == 128 {
            return Err(RegistrationError::Limit);
        }
        self.last = self.last.checked_add(1).ok_or(RegistrationError::Limit)?;
        let next = Instant::now()
            .checked_add(delay)
            .ok_or(RegistrationError::Limit)?;
        let id = TimerId(self.last);
        self.timers.insert(
            id,
            Timer {
                room,
                next,
                interval,
            },
        );
        Ok(id)
    }

    pub fn cancel(&mut self, timer: TimerId) -> bool {
        self.timers.remove(&timer).is_some()
    }
    pub fn clear(&mut self) {
        self.timers.clear();
    }

    async fn next(&mut self) -> Scheduled {
        let Some((&id, timer)) = self
            .timers
            .iter()
            .min_by_key(|(id, timer)| (timer.next, **id))
        else {
            return std::future::pending().await;
        };
        tokio::time::sleep_until(timer.next).await;
        let event = Scheduled {
            timer: id,
            room: timer.room,
        };
        if let Some(next) = timer
            .interval
            .and_then(|interval| Instant::now().checked_add(interval))
        {
            self.timers
                .get_mut(&id)
                .expect("schedule owns this timer")
                .next = next;
        } else {
            self.timers.remove(&id);
        }
        event
    }
}

/// Compose a schedule with an existing event source. There is no worker task
/// per timer; dropping or shutting down the source drops all due events.
pub struct ScheduledSource<S> {
    pub source: S,
    pub schedule: Schedule,
}

impl<S: EventSource> EventSource for ScheduledSource<S> {
    fn recv(
        &mut self,
        cancel: watch::Receiver<bool>,
    ) -> BoxFuture<'_, Result<Option<Event>, SourceError>> {
        Box::pin(async move {
            let mut stopped = cancel.clone();
            tokio::select! { biased;
                _ = crate::runtime::cancelled(&mut stopped) => Ok(None),
                event = self.schedule.next() => Ok(Some(Event::Scheduled(event))),
                event = self.source.recv(cancel) => event,
            }
        })
    }

    fn shutdown(&mut self) -> BoxFuture<'_, Result<(), SourceError>> {
        self.schedule.clear();
        self.source.shutdown()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn timers_skip_missed_intervals_and_honor_cancellation_and_scope() {
        let room = RoomId::new(1).unwrap();
        let mut schedule = Schedule::new(&[room]).unwrap();
        let delay = Duration::from_millis(10);
        assert!(schedule.add(RoomId::new(2).unwrap(), delay, None).is_err());
        let timer = schedule.add(room, delay, Some(delay)).unwrap();
        tokio::time::sleep(delay * 4).await;
        assert_eq!(schedule.next().await, Scheduled { timer, room });
        assert!(
            tokio::time::timeout(Duration::from_millis(1), schedule.next())
                .await
                .is_err()
        );
        assert!(schedule.cancel(timer));
        let (_tx, source) = crate::channel(1).unwrap();
        let mut source = ScheduledSource { source, schedule };
        let (_stop, cancel) = watch::channel(true);
        assert!(source.recv(cancel).await.unwrap().is_none());
        source.shutdown().await.unwrap();
    }
}
