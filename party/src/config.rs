use anyhow::anyhow;
use libp2p::{Multiaddr, PeerId};
use mpc_p2p::{broadcast, MultiaddrWithPeerId};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
pub struct Config {
    pub(crate) parties: Vec<PartyConfig>,
}

impl Config {
    pub fn load_config<P: AsRef<Path>>(path: P) -> Result<Self, anyhow::Error> {
        let file = fs::read_to_string(path)
            .map_err(|e| anyhow!("reading config terminated with err: {}", e))?;
        serde_json::from_str(file.as_str())
            .map_err(|e| anyhow!("decoding config terminated with err: {}", e))
    }

    pub fn addr_of_peer_id(&self, peer: PeerId) -> Option<Multiaddr> {
        self.parties
            .iter()
            .find(|p| p.network_peer.peer_id == peer)
            .map(|p| p.network_peer.multiaddr.clone())
    }
}

#[derive(Serialize, Deserialize)]
pub struct PartyConfig {
    pub(crate) name: String,
    pub(crate) network_peer: MultiaddrWithPeerId,
}
