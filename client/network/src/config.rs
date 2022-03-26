use libp2p::{Multiaddr, PeerId};
use libp2p::identity::Keypair;
use serde::{Deserialize, Serialize};
use crate::broadcast;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Params {
    pub network_config: NetworkConfig,
    pub broadcast_protocols: Vec<broadcast::ProtocolConfig>
}

/// Libp2p config for the Forest node.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct NetworkConfig {
    pub keypair: Keypair,
    pub node_name: String,
    /// Multi-addresses to listen for incoming connections.
    pub listen_addresses: Vec<Multiaddr>,
    /// Multi-addresses to advertise. Detected automatically if empty.
    pub public_addresses: Vec<Multiaddr>,
    /// Configuration for the default set of nodes that participate in computation.
    pub default_peers_set: mpc_peerset::SetConfig,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            node_name: "node".to_string(),
            listen_addresses: vec!["/ip4/0.0.0.0/tcp/0".parse().unwrap()],
            public_addresses: vec!["/ip4/0.0.0.0/tcp/0".parse().unwrap()],
            default_peers_set: mpc_peerset::SetConfig{
                in_peers: 0,
                out_peers: 0,
                bootnodes: vec![],
                reserved_nodes: Default::default(),
                reserved_only: false
            }
        }
    }
}
