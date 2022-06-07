use itertools::Itertools;
use libp2p::PeerId;
use std::io::{BufReader, Read};
use std::ops::Index;

#[derive(Clone, Serialize, Deserialize)]
pub struct Peerset {
    local_peer_id: PeerId,
    peers: Vec<PeerId>,
}

impl Peerset {
    pub fn new(peers: impl Iterator<Item = PeerId>, local_peer_id: PeerId) -> Self {
        Self {
            local_peer_id,
            peers: peers.sorted_by_key(|p| p.to_bytes()).collect(),
        }
    }

    pub fn from_bytes(bytes: &[u8], local_peer_id: PeerId) -> Self {
        let mut peers = vec![];
        let mut reader = BufReader::new(bytes);

        loop {
            let mut buf = [0u8; 38];
            if matches!(reader.read(&mut buf), Ok(n) if n == 38) {
                peers.push(PeerId::from_bytes(&buf).unwrap())
            } else {
                break;
            }
        }

        Self {
            local_peer_id,
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

    pub fn remotes_iter(self) -> impl Iterator<Item = PeerId> {
        self.peers
            .into_iter()
            .filter(move |p| *p != self.local_peer_id)
            .map(|p| p.clone())
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
