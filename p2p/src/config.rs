use libp2p::{Multiaddr, PeerId};
use serde::{Deserialize, Serialize};

/// Libp2p config for the Forest node.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct P2PConfig {
    /// Local address.
    pub host: Multiaddr,
    /// Parties peer list.
    pub parties: Vec<(PeerId, Multiaddr)>,
}

impl Default for P2PConfig {
    fn default() -> Self {
        Self {
            host: "/ip4/0.0.0.0/tcp/0".parse().unwrap(),
            parties: vec![]
        }
    }
}
