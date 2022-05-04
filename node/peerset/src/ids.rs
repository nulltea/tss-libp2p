/// Identifier of a set in the peerset.
///
/// Can be constructed using the `From<usize>` trait implementation based on the index of the set
/// within [`PeersetConfig::sets`]. For example, the first element of [`PeersetConfig::sets`] is
/// later referred to with `SetId::from(0)`. It is intended that the code responsible for building
/// the [`PeersetConfig`] is also responsible for constructing the [`SetId`]s.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SetId(usize);

impl SetId {
    pub const fn from(id: usize) -> Self {
        Self(id)
    }
}

impl From<usize> for SetId {
    fn from(id: usize) -> Self {
        Self(id)
    }
}

impl From<SetId> for usize {
    fn from(id: SetId) -> Self {
        id.0
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

pub enum SetSize {
    Exact(usize),
    AtLeast(usize),
}
