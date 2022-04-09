use anyhow::anyhow;
use libp2p::{Multiaddr, PeerId};
use mpc_p2p::{MultiaddrWithPeerId, NodeKeyConfig, Secret};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::str::FromStr;

#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    pub parties: Vec<PartyConfig>,
}

impl Config {
    pub fn load_config<P: AsRef<Path>>(path: P) -> Result<Self, anyhow::Error> {
        let file = fs::read_to_string(path)
            .map_err(|e| anyhow!("reading config terminated with err: {}", e))?;
        serde_json::from_str(file.as_str())
            .map_err(|e| anyhow!("decoding config terminated with err: {}", e))
    }

    pub fn party_by_peer_id(&self, peer: PeerId) -> Option<&PartyConfig> {
        self.parties.iter().find(|p| p.network_peer.peer_id == peer)
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PartyConfig {
    pub name: String,
    pub network_peer: MultiaddrWithPeerId,
    pub rpc_addr: String,
}

pub fn generate_config(n: u32) -> Result<Config, anyhow::Error> {
    let mut parties = vec![];

    for i in 0..n {
        let node_key = NodeKeyConfig::Ed25519(Secret::File(format!("data/{i}.key").into()));
        let keypair = node_key
            .into_keypair()
            .map_err(|e| anyhow!("keypair generating err: {}", e))?;
        let peer_id = PeerId::from(keypair.public());
        let multiaddr = Multiaddr::from_str(format!("/ip4/127.0.0.1/tcp/400{i}").as_str())
            .map_err(|e| anyhow!("multiaddr parce err: {}", e))?;
        let network_peer = MultiaddrWithPeerId { multiaddr, peer_id };

        parties.push(PartyConfig {
            name: format!("player_{i}"),
            network_peer,
            rpc_addr: format!("127.0.0.1:808{i}"),
        })
    }

    let config = Config { parties };

    let json_bytes = serde_json::to_vec(&config)
        .map_err(|e| anyhow!("config encoding terminated with err: {}", e))?;

    fs::write("config.json", json_bytes.as_slice())
        .map_err(|e| anyhow!("writing config err: {}", e))?;

    Ok(config)
}
