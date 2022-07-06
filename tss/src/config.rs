use anyhow::anyhow;
use libp2p::{Multiaddr, PeerId};
use mpc_p2p::{MultiaddrWithPeerId, NodeKeyConfig, Secret};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::str::FromStr;

#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    pub local: PartyConfig,
    pub boot_peers: Vec<MultiaddrWithPeerId>,
}

impl Config {
    pub fn load_config<P: AsRef<Path>>(path: P) -> Result<Self, anyhow::Error> {
        let file = fs::read_to_string(path)
            .map_err(|e| anyhow!("reading config terminated with err: {}", e))?;
        serde_json::from_str(file.as_str())
            .map_err(|e| anyhow!("decoding config terminated with err: {}", e))
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PartyConfig {
    pub network_peer: MultiaddrWithPeerId,
    pub rpc_addr: String,
}

pub fn generate_config<P: AsRef<Path>, S: AsRef<str>>(
    cfg_path: P,
    setup_path: S,
    libp2p_addr: S,
    rpc_addr: S,
) -> Result<Config, anyhow::Error>
where
    String: From<S>,
{
    let node_key = NodeKeyConfig::Ed25519(Secret::New);
    let keypair = node_key
        .into_keypair()
        .map_err(|e| anyhow!("keypair generating err: {}", e))?;
    let peer_id = PeerId::from(keypair.public());
    let path = setup_path.as_ref().replace(":id", &*peer_id.to_base58());
    let path = Path::new(&path);
    let dir = path.parent().unwrap();
    fs::create_dir_all(dir).unwrap();
    NodeKeyConfig::persist(keypair, path)
        .map_err(|e| anyhow!("secret key backup failed with err: {}", e))?;
    let multiaddr = Multiaddr::from_str(libp2p_addr.as_ref())
        .map_err(|e| anyhow!("multiaddr parse err: {}", e))?;
    let network_peer = MultiaddrWithPeerId { multiaddr, peer_id };

    let config = Config {
        local: PartyConfig {
            network_peer,
            rpc_addr: rpc_addr.into(),
        },
        boot_peers: vec![],
    };

    let json_bytes = serde_json::to_vec(&config)
        .map_err(|e| anyhow!("config encoding terminated with err: {}", e))?;

    fs::write(cfg_path, json_bytes.as_slice()).map_err(|e| anyhow!("writing config err: {}", e))?;

    Ok(config)
}
