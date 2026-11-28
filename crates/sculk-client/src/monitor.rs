//! Optional bounded metadata observer. The transport never waits for a reader.
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    Request,
    Acknowledged,
    Rejected,
    Event,
    Mismatch,
    Timeout,
    WriteFailed,
}

/// Allowlisted operation name and transport-only counters. Bodies are discarded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Entry {
    pub time_ms: u64,
    pub generation: u64,
    pub command: &'static str,
    pub correlation: u32,
    pub packet_bytes: usize,
    pub elapsed: Option<Duration>,
    pub status: Status,
    pub pending: usize,
    pub queued: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Stats {
    pub requests: u64,
    pub acknowledgments: u64,
    pub rejections: u64,
    pub failures: u64,
    pub events: u64,
    pub pending_peak: usize,
    pub queued_peak: usize,
    pub overwritten: u64,
    pub dropped: u64,
    pub generations: u64,
}

struct History {
    entries: VecDeque<Entry>,
    capacity: usize,
    stats: Stats,
}

struct Shared {
    history: Mutex<History>,
    dropped: AtomicU64,
    generations: AtomicU64,
}

/// Caller-owned trace buffer. Enable it explicitly on a connector.
#[derive(Clone)]
pub struct Monitor(Arc<Shared>);

impl Monitor {
    pub fn new(capacity: usize) -> Result<Self, crate::Error> {
        if !(1..=512).contains(&capacity) {
            return Err(crate::Error::Config);
        }
        Ok(Self(Arc::new(Shared {
            history: Mutex::new(History {
                entries: VecDeque::with_capacity(capacity),
                capacity,
                stats: Stats::default(),
            }),
            dropped: AtomicU64::new(0),
            generations: AtomicU64::new(0),
        })))
    }

    pub fn snapshot(&self) -> (Stats, Vec<Entry>) {
        let history = self.0.history.lock().unwrap_or_else(|err| err.into_inner());
        let mut stats = history.stats;
        stats.dropped = self.0.dropped.load(Ordering::Relaxed);
        stats.generations = self.0.generations.load(Ordering::Relaxed);
        (stats, history.entries.iter().copied().collect())
    }

    pub(crate) fn next(&self) -> Observer {
        let generation = self
            .0
            .generations
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        Observer {
            monitor: self.clone(),
            generation,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Observer {
    monitor: Monitor,
    generation: u64,
}

impl Observer {
    pub(crate) fn record(
        &self,
        command: sculk_protocol::Command,
        packet: (u32, usize),
        status: Status,
        elapsed: Option<Duration>,
        depth: (usize, usize),
    ) {
        let Ok(mut history) = self.monitor.0.history.try_lock() else {
            self.monitor.0.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let stats = &mut history.stats;
        let count = match status {
            Status::Request => &mut stats.requests,
            Status::Acknowledged => &mut stats.acknowledgments,
            Status::Rejected => &mut stats.rejections,
            Status::Event => &mut stats.events,
            _ => &mut stats.failures,
        };
        *count = count.saturating_add(1);
        stats.pending_peak = stats.pending_peak.max(depth.0);
        stats.queued_peak = stats.queued_peak.max(depth.1);
        if history.entries.len() == history.capacity {
            history.entries.pop_front();
            history.stats.overwritten = history.stats.overwritten.saturating_add(1);
        }
        let command = match command.as_str() {
            "GETCONF" => "GETCONF",
            "CHECKIN" => "CHECKIN",
            "LOGINLIST" => "LOGINLIST",
            "PING" => "PING",
            "WRITE" => "WRITE",
            "MSG" => "MSG",
            "LCHATLIST" => "LCHATLIST",
            "CHATINFO" => "CHATINFO",
            "GETMEM" => "GETMEM",
            "INFOLINK" => "INFOLINK",
            "SYNCMSG" => "SYNCMSG",
            "NEWMEM" => "NEWMEM",
            "DELMEM" => "DELMEM",
            _ => "OTHER",
        };
        let time_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_millis()).ok())
            .unwrap_or(0);
        history.entries.push_back(Entry {
            time_ms,
            generation: self.generation,
            command,
            correlation: packet.0,
            packet_bytes: packet.1,
            elapsed,
            status,
            pending: depth.0,
            queued: depth.1,
        });
    }
}
