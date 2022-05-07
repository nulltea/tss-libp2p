use crate::broadcast;
use anyhow::anyhow;
use libp2p::identity::{ed25519, Keypair};
use libp2p::{multiaddr, Multiaddr, PeerId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::error::Error;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::{fmt, fs, io};
use zeroize::Zeroize;

pub struct Params {
    pub network_config: NetworkConfig,
    pub broadcast_protocols: Vec<broadcast::ProtocolConfig>,
}

#[derive(Clone)]
pub struct NetworkConfig {
    /// Multi-addresses to listen for incoming connections.
    pub listen_address: Multiaddr,
    /// Mdns discovery enabled.
    pub mdns: bool,
    /// Kademlia discovery enabled.
    pub kademlia: bool,
    /// Rooms
    pub rooms: Vec<RoomConfig>,
}

pub struct RoomConfig {
    pub name: String,

    pub target_size: usize,

    /// Configuration for the default set of nodes that participate in computation.
    pub boot_peers: Vec<MultiaddrWithPeerId>,
}

/// The configuration of a node's secret key, describing the type of key
/// and how it is obtained. A node's identity keypair is the result of
/// the evaluation of the node key configuration.
#[derive(Clone)]
pub enum NodeKeyConfig {
    /// A Ed25519 secret key configuration.
    Ed25519(Secret<ed25519::SecretKey>),
}

impl Default for NodeKeyConfig {
    fn default() -> NodeKeyConfig {
        Self::Ed25519(Secret::New)
    }
}

/// The configuration options for obtaining a secret key `K`.
#[derive(Clone)]
pub enum Secret<K> {
    /// Use the given secret key `K`.
    Input(K),
    /// Read the secret key from a file. If the file does not exist,
    /// it is created with a newly generated secret key `K`. The format
    /// of the file is determined by `K`:
    ///
    ///   * `ed25519::SecretKey`: An unencoded 32 bytes Ed25519 secret key.
    File(PathBuf),
    /// Always generate a new secret key `K`.
    New,
}

impl NodeKeyConfig {
    /// Evaluate a `NodeKeyConfig` to obtain an identity `Keypair`.
    pub fn into_keypair(self) -> io::Result<Keypair> {
        use NodeKeyConfig::*;
        match self {
            Ed25519(Secret::New) => Ok(Keypair::generate_ed25519()),
            Ed25519(Secret::Input(k)) => Ok(Keypair::Ed25519(k.into())),
            Ed25519(Secret::File(f)) => get_secret(
                f,
                |mut b| match String::from_utf8(b.to_vec()).ok().and_then(|s| {
                    if s.len() == 64 {
                        hex::decode(&s).ok()
                    } else {
                        None
                    }
                }) {
                    Some(s) => ed25519::SecretKey::from_bytes(s),
                    _ => ed25519::SecretKey::from_bytes(&mut b),
                },
                ed25519::SecretKey::generate,
                |b| b.as_ref().to_vec(),
            )
            .map(ed25519::Keypair::from)
            .map(Keypair::Ed25519),
        }
    }
}

/// Address of a node, including its identity.
///
/// This struct represents a decoded version of a multiaddress that ends with `/p2p/<peerid>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MultiaddrWithPeerId {
    /// Address of the node.
    pub multiaddr: Multiaddr,
    /// Its identity.
    pub peer_id: PeerId,
}

impl MultiaddrWithPeerId {
    /// Concatenates the multiaddress and peer ID into one multiaddress containing both.
    pub fn concat(&self) -> Multiaddr {
        let proto = multiaddr::Protocol::P2p(From::from(self.peer_id));
        self.multiaddr.clone().with(proto)
    }
}

impl fmt::Display for MultiaddrWithPeerId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&self.concat(), f)
    }
}

impl FromStr for MultiaddrWithPeerId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (peer_id, multiaddr) = parse_str_addr(s)?;
        Ok(Self { peer_id, multiaddr })
    }
}

impl From<MultiaddrWithPeerId> for String {
    fn from(ma: MultiaddrWithPeerId) -> String {
        format!("{}", ma)
    }
}

impl TryFrom<String> for MultiaddrWithPeerId {
    type Error = anyhow::Error;
    fn try_from(string: String) -> Result<Self, Self::Error> {
        string
            .parse()
            .map_err(|e| anyhow!("parsing multiaddr_peer_id terminated with err: {}", e))
    }
}

/// Parses a string address and splits it into Multiaddress and PeerId, if
/// valid.
pub fn parse_str_addr(addr_str: &str) -> Result<(PeerId, Multiaddr), anyhow::Error> {
    let addr: Multiaddr = addr_str.parse()?;
    parse_addr(addr)
}

/// Splits a Multiaddress into a Multiaddress and PeerId.
pub fn parse_addr(mut addr: Multiaddr) -> Result<(PeerId, Multiaddr), anyhow::Error> {
    let who = match addr.pop() {
        Some(multiaddr::Protocol::P2p(key)) => {
            PeerId::from_multihash(key).map_err(|_| anyhow!("invalid peer id"))?
        }
        _ => return Err(anyhow!("peer id missing")),
    };

    Ok((who, addr))
}

// impl Eq for MultiaddrWithPeerId {}
//
// impl PartialEq<Self> for MultiaddrWithPeerId {
//     fn eq(&self, other: &Self) -> bool {
//         self.peer_id == other.peer_id
//     }
// }
//
// impl PartialOrd<Self> for MultiaddrWithPeerId {
//     fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
//         todo!()
//     }
// }
//
// impl Ord for MultiaddrWithPeerId {
//     fn cmp(&self, other:&Self) -> Ordering {
//         let size1 = self.peer_id;
//         let size2 = other.value;
//         if self > size2 {
//             Ordering::Less
//         }
//         if size1 < size2 {
//             Ordering::Greater
//         }
//         Ordering::Equal
//     }
// }

/// Load a secret key from a file, if it exists, or generate a
/// new secret key and write it to that file. In either case,
/// the secret key is returned.
fn get_secret<P, F, G, E, W, K>(file: P, parse: F, generate: G, serialize: W) -> io::Result<K>
where
    P: AsRef<Path>,
    F: for<'r> FnOnce(&'r mut [u8]) -> Result<K, E>,
    G: FnOnce() -> K,
    E: Error + Send + Sync + 'static,
    W: Fn(&K) -> Vec<u8>,
{
    std::fs::read(&file)
        .and_then(|mut sk_bytes| {
            parse(&mut sk_bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        })
        .or_else(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                file.as_ref().parent().map_or(Ok(()), fs::create_dir_all)?;
                let sk = generate();
                let mut sk_vec = serialize(&sk);
                write_secret_file(file, &sk_vec)?;
                sk_vec.zeroize();
                Ok(sk)
            } else {
                Err(e)
            }
        })
}

/// Write secret bytes to a file.
fn write_secret_file<P>(path: P, sk_bytes: &[u8]) -> io::Result<()>
where
    P: AsRef<Path>,
{
    let mut file = open_secret_file(&path)?;
    file.write_all(sk_bytes)
}

/// Opens a file containing a secret key in write mode.
#[cfg(unix)]
fn open_secret_file<P>(path: P) -> io::Result<fs::File>
where
    P: AsRef<Path>,
{
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}
