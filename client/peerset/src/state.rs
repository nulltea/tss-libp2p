// This file is part of Substrate.

// Copyright (C) 2019-2021 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use libp2p::PeerId;
use log::error;
use std::{
    borrow::Cow,
    collections::{
        hash_map::{Entry, OccupiedEntry},
        HashMap, HashSet,
    },
    time::Instant,
};
use crate::{ConnectedPeer, NotConnectedPeer, Peer, Reputation, PeersetConfig, SetInfo, UnknownPeer};

/// State storage behind the peerset.
///
/// # Usage
///
/// This struct is nothing more but a data structure containing a list of nodes, where each node
/// has a reputation and is either connected to us or not.
#[derive(Debug, Clone)]
pub struct PeersState {
    /// List of nodes that we know about.
    ///
    /// > **Note**: This list should really be ordered by decreasing reputation, so that we can
    ///           easily select the best node to connect to. As a first draft, however, we don't
    ///           sort, to make the logic easier.
    pub(crate) nodes: HashMap<PeerId, Node>,

    /// Configuration of the set.
    pub(crate) set: SetInfo,
}

/// State of a single node that we know about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Node {
    /// List of sets the node belongs to.
    /// Always has a fixed size equal to the one of [`PeersState::set`]. The various possible sets
    /// are indices into this `Vec`.
    pub(crate) set: MembershipState,

    /// Reputation value of the node, between `i32::MIN` (we hate that node) and
    /// `i32::MAX` (we love that node).
    pub(crate) reputation: i32,
}

impl Node {
    pub fn new() -> Self {
        Self {
            set: MembershipState::NotMember,
            reputation: 0,
        }
    }
}

impl PeersState {
    /// Builds a new empty [`PeersState`].
    pub fn new(config: PeersetConfig) -> Self {
        Self {
            nodes: HashMap::new(),
            set: SetInfo {
                num_peers: 0,
                max_peers: config.peers_limit,
                initial_nodes: config.initial_nodes,
                static_set: config.static_set,
            },
        }
    }

    /// Returns an object that grants access to the reputation value of a peer.
    pub fn peer_reputation(&mut self, peer_id: PeerId) -> Reputation {
        if !self.nodes.contains_key(&peer_id) {
            self.nodes.insert(peer_id, Node::new());
        }

        let entry = match self.nodes.entry(peer_id) {
            Entry::Vacant(_) => unreachable!("guaranteed to be inserted above; qed"),
            Entry::Occupied(e) => e,
        };

        Reputation { node: Some(entry) }
    }

    /// Returns an object that grants access to the state of a peer in the context of the set.
    pub fn peer<'a>(&'a mut self, peer_id: &'a PeerId) -> Peer<'a> {
        match self.nodes.get(peer_id).map(|p| &p.set) {
            None | Some(MembershipState::NotMember) => Peer::Unknown(UnknownPeer {
                parent: self,
                peer_id: Cow::Borrowed(peer_id),
            }),
            Some(MembershipState::Connected) => Peer::Connected(ConnectedPeer {
                state: self,
                peer_id: Cow::Borrowed(peer_id),
            }),
            Some(MembershipState::NotConnected { .. }) => Peer::NotConnected(NotConnectedPeer {
                state: self,
                peer_id: Cow::Borrowed(peer_id),
            }),
        }
    }

    /// Returns the list of all the peers we know of.
    // Note: this method could theoretically return a `Peer`, but implementing that
    // isn't simple.
    pub fn peer_ids(&self) -> impl ExactSizeIterator<Item = &PeerId> {
        self.nodes.keys()
    }

    /// Returns the index of a specified peer in a given set.
    pub fn index_of(&self, peer: PeerId) -> Option<usize> {
        self.connected_peers()
            .position(|elem| {
                let p = (*elem);
                let b = p == peer;
                b
            })
    }

    /// Returns the index of a specified peer in a given set.
    pub fn at_index(&self, index: usize) -> Option<&PeerId> {
        let peers: Vec<&PeerId> = self.connected_peers().collect();

        if peers.len() > index {
            Some(peers[index])
        } else {
            None
        }
    }

    /// Returns the list of peers we are connected to in the context of the set.
    pub fn connected_peers(&self) -> impl Iterator<Item = &PeerId> {
        self.nodes
            .iter()
            .filter(move |(_, p)| p.set.is_connected())
            .map(|(p, _)| p)
    }

    /// Returns the peer with the highest reputation and that we are not connected to.
    ///
    /// If multiple nodes have the same reputation, which one is returned is unspecified.
    pub fn highest_not_connected_peer(&mut self) -> Option<NotConnectedPeer> {
        let outcome = self
            .nodes
            .iter_mut()
            .filter(|(_, Node { set, .. })| set.is_not_connected())
            .fold(None::<(&PeerId, &mut Node)>, |mut cur_node, to_try| {
                if let Some(cur_node) = cur_node.take() {
                    if cur_node.1.reputation >= to_try.1.reputation {
                        return Some(cur_node);
                    }
                }
                Some(to_try)
            })
            .map(|(peer_id, _)| *peer_id);

        outcome.map(move |peer_id| NotConnectedPeer {
            state: self,
            peer_id: Cow::Owned(peer_id),
        })
    }
}

/// Whether we are connected to a node in the context of a specific set.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum MembershipState {
    /// Node isn't part of that set.
    NotMember,
    /// We are connected through an ingoing connection.
    Connected,
    /// Node is part of that set, but we are not connected to it.
    NotConnected {
        /// When we were last connected to the node, or if we were never connected when we
        /// discovered it.
        last_connected: Instant,
    },
}

impl MembershipState {
    /// Returns `true` for [`MembershipState::Connected`].
    fn is_connected(self) -> bool {
        match self {
            Self::Connected => true,
            Self::NotMember | Self::NotConnected { .. } => false,
        }
    }

    /// Returns `true` for [`MembershipState::NotConnected`].
    fn is_not_connected(self) -> bool {
        matches!(self, Self::NotConnected { .. })
    }
}
