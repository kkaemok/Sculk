use crate::{RegistrationError, RoomId, RoomMemberId};
use std::collections::HashSet;

/// Local dispatch policy, scoped by exact room and member identities.
#[derive(Default)]
pub struct Blocklist(HashSet<(RoomId, RoomMemberId)>);

impl Blocklist {
    pub fn insert(
        &mut self,
        room: RoomId,
        member: RoomMemberId,
    ) -> Result<bool, RegistrationError> {
        let key = (room, member);
        if self.0.len() == 4096 && !self.0.contains(&key) {
            return Err(RegistrationError::Limit);
        }
        Ok(self.0.insert(key))
    }

    pub fn remove(&mut self, room: RoomId, member: RoomMemberId) -> bool {
        self.0.remove(&(room, member))
    }

    pub fn contains(&self, room: RoomId, member: RoomMemberId) -> bool {
        self.0.contains(&(room, member))
    }
}
