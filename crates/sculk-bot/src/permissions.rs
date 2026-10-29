//! Application permissions, independent of KakaoTalk room roles.
use crate::{RoomId, SenderId};
use std::{collections::HashMap, sync::Arc};

/// Registered application permission name. Does not represent a server role.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PermissionId(Arc<str>);
impl PermissionId {
    /// Local grammar: 1..64 ASCII letters, digits, `.`, `_`, `-`.
    pub fn new(name: &str) -> Result<Self, crate::RegistrationError> {
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(crate::RegistrationError::Invalid);
        }
        Ok(Self(name.into()))
    }
    /// Application-defined identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
/// Explicit local override. Applicable denials always take precedence over grants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Grant,
    Deny,
}
/// Small synchronous lookup boundary for future application/script stores. Must
/// not block or perform disk/network I/O in this method. None uses the default.
pub trait PermissionStore: Send + Sync {
    /// Global and selected-room overrides must already be combined, deny first.
    fn decision(
        &self,
        permission: &PermissionId,
        room: RoomId,
        sender: SenderId,
    ) -> Option<Decision>;
}
/// Bounded startup-configured memory store. Identifiers are room-scoped in the
/// client; a global override is an explicit operator choice across numeric IDs.
#[derive(Default)]
pub struct MemoryPermissions {
    rules: HashMap<(PermissionId, SenderId, Option<RoomId>), Decision>,
}
impl MemoryPermissions {
    /// At most 4096 distinct rules. Replacing an existing rule uses no extra slot.
    pub fn set(
        &mut self,
        permission: PermissionId,
        sender: SenderId,
        room: Option<RoomId>,
        decision: Decision,
    ) -> Result<(), crate::RegistrationError> {
        let key = (permission, sender, room);
        if self.rules.len() == 4096 && !self.rules.contains_key(&key) {
            return Err(crate::RegistrationError::Limit);
        }
        self.rules.insert(key, decision);
        Ok(())
    }
}
impl PermissionStore for MemoryPermissions {
    fn decision(
        &self,
        permission: &PermissionId,
        room: RoomId,
        sender: SenderId,
    ) -> Option<Decision> {
        let global = self.rules.get(&(permission.clone(), sender, None)).copied();
        let local = self
            .rules
            .get(&(permission.clone(), sender, Some(room)))
            .copied();
        match (global, local) {
            (Some(Decision::Deny), _) | (_, Some(Decision::Deny)) => Some(Decision::Deny),
            (Some(Decision::Grant), _) | (_, Some(Decision::Grant)) => Some(Decision::Grant),
            _ => None,
        }
    }
}
pub(crate) struct Permissions {
    pub defaults: HashMap<PermissionId, bool>,
    pub store: Arc<dyn PermissionStore>,
}
impl Permissions {
    pub fn allows(&self, id: &PermissionId, room: RoomId, sender: SenderId) -> bool {
        let Some(default) = self.defaults.get(id) else {
            return false;
        };
        match self.store.decision(id, room, sender) {
            Some(Decision::Grant) => true,
            Some(Decision::Deny) => false,
            None => *default,
        }
    }
}
