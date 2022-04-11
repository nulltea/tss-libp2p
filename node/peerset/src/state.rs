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

use crate::{
    ConnectedPeer, MembershipState, NotConnectedPeer, Peer, PeersetConfig, SetInfo, UnknownPeer,
};
use itertools::Itertools;
use libp2p::PeerId;
use std::{borrow::Cow, collections::HashMap};

/// State storage behind the peerset.
///
/// # Usage
///
/// This struct is nothing more but a data structure containing a list of nodes, where each node
/// is either connected to us or not.
#[derive(Debug, Clone)]
pub struct PeersState {
    local_peer_id: PeerId,
    /// List of nodes that we know about.
    pub(crate) nodes: HashMap<PeerId, MembershipState>,

    /// Configuration of the set.
    pub(crate) set: SetInfo,
}

impl PeersState {
    /// Builds a new empty [`PeersState`].
    pub fn new(local_peer_id: PeerId, config: PeersetConfig) -> Self {
        Self {
            local_peer_id,
            nodes: HashMap::new(),
            set: SetInfo {
                num_peers: 0,
                max_peers: config.peers_limit,
                initial_nodes: config.initial_nodes,
                static_set: config.static_set,
            },
        }
    }

    /// Returns an object that grants access to the state of a peer in the context of the set.
    pub fn peer<'a>(&'a mut self, peer_id: &'a PeerId) -> Peer<'a> {
        match self.nodes.get(peer_id).map(|s| *s) {
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
    pub fn peer_ids(&self) -> impl ExactSizeIterator<Item = PeerId> + '_ {
        self.nodes.keys().map(|p| p.clone())
    }

    /// Returns the index of a specified peer in a given set.
    pub fn index_of(&self, peer: PeerId) -> Option<usize> {
        self.nodes
            .iter()
            .map(|(p, _)| p)
            .sorted_by_key(|p| p.to_bytes())
            .position(|elem| *elem == peer)
    }

    /// Returns the index of a specified peer in a given set.
    pub fn at_index(&self, index: usize) -> Option<PeerId> {
        let peers: Vec<&PeerId> = self
            .nodes
            .iter()
            .map(|(p, _)| p)
            .sorted_by_key(|p| p.to_bytes())
            .collect();

        if peers.len() > index {
            Some(peers[index].clone())
        } else {
            None
        }
    }

    /// Returns the list of peers we are connected to in the context of the set.
    pub fn connected_peers(&self) -> impl Iterator<Item = &PeerId> {
        self.nodes
            .iter()
            .filter(move |(p, n)| n.is_connected() && p.to_bytes() != self.local_peer_id.to_bytes())
            .map(|(p, _)| p)
    }

    /// Returns peer's membership state in the set.
    pub fn peer_membership(&self, peer_id: &PeerId) -> MembershipState {
        self.nodes
            .iter()
            .find(move |(p, _)| p.to_bytes() == peer_id.to_bytes())
            .map(|(_, s)| *s)
            .unwrap_or(MembershipState::NotMember)
    }
}
