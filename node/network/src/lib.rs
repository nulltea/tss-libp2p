#![feature(inherent_associated_types)]

extern crate core;

mod behaviour;
pub mod broadcast;
mod config;
mod discovery;
mod error;
mod messages;
mod service;

pub use self::config::*;
pub use self::messages::*;
pub use self::service::*;
use std::borrow::Cow;

use arrayvec::ArrayString;

/// The maximum allowed number of established connections per peer.
const MAX_CONNECTIONS_PER_PEER: usize = 2;

/// Identifier of a room in the peerset.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RoomId(ArrayString<64>);

impl RoomId {
    pub fn from(id: String) -> Self {
        Self(blake3::hash(id.as_bytes()).to_hex())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn as_protocol_id(&self) -> Cow<'static, str> {
        Cow::Owned(format!("/room/{}", self.0.to_string()))
    }
}
