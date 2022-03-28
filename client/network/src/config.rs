use crate::broadcast;
use libp2p::{Multiaddr, PeerId};
use serde::{Deserialize, Serialize};

pub struct Params {
    pub network_config: NetworkConfig,
    pub broadcast_protocols: Vec<broadcast::ProtocolConfig>,
}

/// Libp2p config for the Forest node.
pub struct NetworkConfig {
    pub node_name: String,
    /// Multi-addresses to listen for incoming connections.
    pub listen_addresses: Vec<Multiaddr>,
    /// Multi-addresses to advertise. Detected automatically if empty.
    pub public_addresses: Vec<Multiaddr>,
    /// Configuration for the default set of nodes that participate in computation.
    pub default_peers_set: mpc_peerset::SetConfig,
}

impl NetworkConfig {
    pub fn new(peers: impl Iterator<Item = PeerId>) -> Self {
        Self {
            node_name: "node".to_string(),
            listen_addresses: vec!["/ip4/0.0.0.0/tcp/0".parse().unwrap()],
            public_addresses: vec!["/ip4/0.0.0.0/tcp/0".parse().unwrap()],
            default_peers_set: mpc_peerset::SetConfig::new_static(peers),
        }
    }
}
