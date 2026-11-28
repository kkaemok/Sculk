//! Bounded optional cache for display data from scoped read-only snapshots.
use crate::read_only::{Error, Member, MemberSnapshot, RoomId, RoomMemberId};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

struct Room {
    expires: Instant,
    members: HashMap<RoomMemberId, Member>,
}

/// A caller-owned cache. Expired observations cannot supply mention or
/// display data. Cache entries confer no server-side permission.
pub struct Cache {
    rooms: HashMap<RoomId, Room>,
    entries: usize,
    capacity: usize,
    ttl: Duration,
}

impl Cache {
    pub fn new(capacity: usize, ttl: Duration) -> Result<Self, Error> {
        if !(1..=4096).contains(&capacity) || ttl.is_zero() || ttl > Duration::from_secs(3600) {
            return Err(Error::InvalidRequest);
        }
        Ok(Self {
            rooms: HashMap::new(),
            entries: 0,
            capacity,
            ttl,
        })
    }

    /// Replace one room atomically after validating a complete observation.
    /// An incomplete attempt invalidates the preceding data for that room.
    pub fn replace(&mut self, snapshot: MemberSnapshot, now: Instant) -> Result<(), Error> {
        let members = match snapshot {
            MemberSnapshot::Complete(members) => members,
            MemberSnapshot::Partial { members, .. } => {
                self.invalidate(members.room);
                return Err(Error::IncompleteSnapshot);
            }
        };
        self.expire(now);
        let previous = self
            .rooms
            .get(&members.room)
            .map_or(0, |room| room.members.len());
        let next = self.entries - previous + members.members.len();
        if next > self.capacity
            || (self.rooms.len() == 128 && !self.rooms.contains_key(&members.room))
        {
            return Err(Error::InvalidRequest);
        }
        let expires = now.checked_add(self.ttl).ok_or(Error::InvalidRequest)?;
        let mut by_id = HashMap::new();
        for member in members.members {
            if member
                .nickname
                .as_ref()
                .is_some_and(|name| name.len() > 1024)
                || by_id.insert(member.id()?, member).is_some()
            {
                return Err(Error::InvalidResponse);
            }
        }
        self.rooms.insert(
            members.room,
            Room {
                expires,
                members: by_id,
            },
        );
        self.entries = next;
        Ok(())
    }

    /// Lookup by exact ID within one room.
    pub fn get(&self, room: RoomId, id: RoomMemberId, now: Instant) -> Option<&Member> {
        self.active(room, now)?.members.get(&id)
    }

    /// Resolve one exact display name. Duplicate names are ambiguous.
    pub fn resolve_name(
        &self,
        room: RoomId,
        name: &str,
        now: Instant,
    ) -> Result<Option<RoomMemberId>, Error> {
        let Some(room) = self.active(room, now) else {
            return Ok(None);
        };
        let mut matches = room
            .members
            .iter()
            .filter(|(_, member)| member.nickname.as_deref() == Some(name));
        let first = matches.next().map(|(id, _)| *id);
        if matches.next().is_some() {
            return Err(Error::InvalidRequest);
        }
        Ok(first)
    }

    fn active(&self, room: RoomId, now: Instant) -> Option<&Room> {
        self.rooms.get(&room).filter(|room| room.expires > now)
    }

    pub fn invalidate(&mut self, room: RoomId) {
        if let Some(room) = self.rooms.remove(&room) {
            self.entries -= room.members.len();
        }
    }

    /// Invalidate before refreshing following a scoped membership observation.
    #[cfg(feature = "messaging")]
    pub fn observe(&mut self, change: crate::messaging::MembershipChange) {
        self.invalidate(change.room);
    }

    fn expire(&mut self, now: Instant) {
        self.rooms.retain(|_, room| room.expires > now);
        self.entries = self.rooms.values().map(|room| room.members.len()).sum();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read_only::Members;

    #[test]
    fn scoped_lookups_expire_and_invalidation_preserves_other_rooms() {
        let room = RoomId::new(1).unwrap();
        let other = RoomId::new(2).unwrap();
        let id = RoomMemberId::new(9_007_199_254_740_993).unwrap();
        let now = Instant::now();
        let ttl = Duration::from_secs(2);
        let snapshot = |room, nick: &str| {
            MemberSnapshot::Complete(Members {
                room,
                token: 0,
                members: vec![Member {
                    user_id: id.get(),
                    nickname: Some(nick.to_owned()),
                }],
            })
        };
        let mut cache = Cache::new(2, ttl).unwrap();
        cache.replace(snapshot(room, "first"), now).unwrap();
        cache.replace(snapshot(other, "second"), now).unwrap();
        assert_eq!(cache.resolve_name(room, "second", now).unwrap(), None);
        assert_eq!(cache.resolve_name(other, "second", now).unwrap(), Some(id));
        cache.invalidate(room);
        assert!(cache.get(room, id, now).is_none());
        assert!(cache.get(other, id, now).is_some());
        assert!(cache.get(other, id, now + ttl).is_none());
    }
}
