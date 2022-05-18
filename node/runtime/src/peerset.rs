use itertools::Itertools;
use libp2p::PeerId;

use std::io::{BufReader, Read};
use std::ops::Index;

#[derive(Clone)]
pub struct Peerset {
    peers: Vec<PeerId>,
}

impl Peerset {
    pub fn new(peers: impl Iterator<Item = PeerId>) -> Self {
        Self {
            peers: peers.sorted_by_key(|p| p.to_bytes()).collect(),
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut peers = vec![];
        let mut reader = BufReader::new(bytes);

        loop {
            let mut buf = [0u8; 32];
            if matches!(reader.read(&mut buf), Ok(n) if n == 32) {
                peers.push(PeerId::from_bytes(&buf).unwrap())
            } else {
                break;
            }
        }

        Self {
            peers: peers.into_iter().sorted_by_key(|p| p.to_bytes()).collect(),
        }
    }

    pub fn index_of(&self, peer_id: &PeerId) -> Option<u16> {
        self.peers
            .iter()
            .position(|elem| *elem == *peer_id)
            .map(|i| i as u16)
    }

    pub fn size(&self) -> usize {
        self.peers.len()
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = vec![];

        for peer_id in &self.peers {
            buf.append(&mut peer_id.to_bytes());
        }

        buf
    }
}

impl Index<u16> for Peerset {
    type Output = PeerId;

    fn index(&self, index: u16) -> &Self::Output {
        &self.peers[index as usize]
    }
}

impl IntoIterator for Peerset {
    type Item = PeerId;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.peers.into_iter()
    }
}
