mod behaviour;
pub mod broadcast;
mod config;
mod coordination;
mod discovery;
mod error;
mod service;

pub use self::config::*;
pub use self::service::*;

/// The maximum allowed number of established connections per peer.
const MAX_CONNECTIONS_PER_PEER: usize = 2;
