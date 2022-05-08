use arrayvec::ArrayString;
use std::cmp::Ordering;

/// Identifier of a room in the peerset.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RoomId(ArrayString<64>);

impl RoomId {
    pub const fn from(id: String) -> Self {
        Self(blake3::hash(id.as_bytes()).to_hex())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn as_protocol_id(&self) -> &str {
        format!("/room/{}", self.as_str()).as_str()
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(u64);

impl SessionId {
    pub const fn from(id: u64) -> Self {
        Self(id)
    }
}

impl From<u64> for SessionId {
    fn from(id: u64) -> Self {
        Self(id)
    }
}

impl From<SessionId> for u64 {
    fn from(id: SessionId) -> Self {
        id.0
    }
}
