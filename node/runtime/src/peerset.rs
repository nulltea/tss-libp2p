use itertools::Itertools;
use libp2p::PeerId;
use log::warn;
use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Read};
use std::ops::Index;

#[derive(Clone)]
pub struct Peerset {
    local_peer_id: PeerId,
    room_peers: Vec<PeerId>,
    pub(crate) session_peers: Vec<usize>,
}

impl Peerset {
    pub fn new(peers: impl Iterator<Item = PeerId>, local_peer_id: PeerId) -> Self {
        let peers: Vec<_> = peers.sorted_by_key(|p| p.to_bytes()).collect();

        Self {
            local_peer_id,
            session_peers: (0..peers.len()).collect(),
            room_peers: peers,
        }
    }

    pub fn from_cache(cache: Self, peers: impl Iterator<Item = PeerId>) -> Self {
        let mut session_peers = vec![];
        for peer_id in peers.sorted_by_key(|p| p.to_bytes()) {
            match cache.index_of(&peer_id) {
                Some(i) => {
                    session_peers.push(i as usize);
                }
                None => {
                    warn!(
                        "Peer {} does not appear in the peerset cache, skipping.",
                        peer_id.to_base58()
                    )
                }
            }
        }

        Self {
            local_peer_id: cache.local_peer_id,
            room_peers: cache.room_peers,
            session_peers,
        }
    }

    pub fn from_bytes(bytes: &[u8], local_peer_id: PeerId) -> Self {
        let mut peers = vec![];
        let mut active_peers = vec![];
        let mut reader = BufReader::new(bytes);

        loop {
            let mut buf = [0; 38];
            if matches!(reader.read(&mut buf), Ok(n) if n == 38) {
                peers.push(PeerId::from_bytes(&buf).unwrap())
            } else {
                break;
            }

            let mut buf = [0; 1];
            reader.read(&mut buf).unwrap();
            if buf[0] == 1 {
                active_peers.push(peers.last().unwrap().clone());
            }
        }

        let peers: Vec<_> = peers.into_iter().sorted_by_key(|p| p.to_bytes()).collect();
        let mut active_indexes = vec![];
        for peer_id in active_peers {
            active_indexes.push(peers.iter().position(|elem| *elem == peer_id).unwrap());
        }

        Self {
            local_peer_id,
            room_peers: peers,
            session_peers: active_indexes,
        }
    }

    pub fn index_of(&self, peer_id: &PeerId) -> Option<u16> {
        self.room_peers
            .iter()
            .position(|elem| *elem == *peer_id)
            .filter(|i| self.session_peers.contains(i))
            .map(|i| i as u16)
    }

    pub fn size(&self) -> usize {
        self.room_peers.len()
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = vec![];

        for (i, peer_id) in self.room_peers.iter().enumerate() {
            buf.append(&mut peer_id.to_bytes());
            buf.push(self.session_peers.contains(&i) as u8);
        }

        buf
    }

    pub fn remotes_iter(self) -> impl Iterator<Item = PeerId> {
        self.room_peers
            .into_iter()
            .filter(move |p| *p != self.local_peer_id)
            .map(|p| p.clone())
    }
}

impl Index<u16> for Peerset {
    type Output = PeerId;

    fn index(&self, index: u16) -> &Self::Output {
        &self.room_peers[index as usize]
    }
}

impl IntoIterator for Peerset {
    type Item = PeerId;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.room_peers.into_iter()
    }
}
