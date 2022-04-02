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

    pub fn sort_parties(&mut self) {
        self.parties.sort_by_key(|p| p.network_peer.peer_id);
    }

    pub fn index_of_party(&self, peer: PeerId) -> usize {
        self.parties
            .iter()
            .position(|p| p.network_peer.peer_id == peer)
            .unwrap()
    }
}

#[derive(Serialize, Deserialize)]
pub struct PartyConfig {
    pub(crate) name: String,
    pub(crate) network_peer: MultiaddrWithPeerId,
}
