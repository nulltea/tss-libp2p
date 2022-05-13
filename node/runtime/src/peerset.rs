use libp2p::PeerId;
use std::collections::HashSet;
use std::ops::Index;

#[derive(Clone)]
pub struct Peerset {
    peers: HashSet<PeerId>,
}

impl Peerset {
    pub fn new(peers: impl Iterator<Item = PeerId>) -> Self {
        Self {
            peers: peers.sorted_by_key(|p| p.to_bytes()).collect(),
        }
    }

    pub fn index_of(&self, peer_id: &PeerId) -> Option<u16> {
        self.peers
            .iter()
            .position(|elem| *elem == *peer_id)
            .map(|i| i as u16)
    }
}

impl Index<u16> for Peerset {
    type Output = PeerId;

    fn index(&self, index: u16) -> &Self::Output {
        self.peers[index as usize]
    }
}

impl IntoIterator for Peerset {
    type Item = PeerId;
    type IntoIter = std::collections::hash_set::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.peers.into_iter()
    }
}
