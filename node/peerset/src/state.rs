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
    ConnectedPeer, MembershipState, NotConnectedPeer, Peer, RoomId, SessionId, SetConfig, SetInfo,
    UnknownPeer,
};
use fnv::FnvHashMap;
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
pub(crate) struct PeersState {
    /// List of nodes that we know about.
    pub(crate) nodes: HashMap<PeerId, Node>,

    /// Configuration of the set.
    pub(crate) rooms: HashMap<RoomId, SetInfo>,

    pub(crate) sessions: HashMap<SessionId, RoomId>,
}

impl PeersState {
    /// Builds a new empty [`PeersState`].
    pub fn new(sets: impl IntoIterator<Item = SetConfig>) -> Self {
        Self {
            nodes: HashMap::default(),
            rooms: sets
                .into_iter()
                .map(|config| SetInfo {
                    num_peers: 0,
                    max_peers: config.target_size,
                    initial_nodes: config.boot_nodes.into_iter().collect(),
                })
                .collect(),
        }
    }

    /// Returns an object that grants access to the state of a peer in the context of the set.
    pub fn peer<'a>(&'a mut self, room_id: &RoomId, peer_id: &'a PeerId) -> Peer<'a> {
        assert!(self.rooms.contains_key(&room_id));

        match self.nodes.get(peer_id).map(|n| *n.rooms.get(&room_id)) {
            None | Some(MembershipState::NotMember) => Peer::Unknown(UnknownPeer {
                room_id: room_id.clone(),
                state: self,
                peer_id: Cow::Borrowed(peer_id),
            }),
            Some(MembershipState::Connected) => Peer::Connected(ConnectedPeer {
                room_id: room_id.clone(),
                state: self,
                peer_id: Cow::Borrowed(peer_id),
            }),
            Some(MembershipState::NotConnected { .. }) => Peer::NotConnected(NotConnectedPeer {
                room_id: room_id.clone(),
                state: self,
                peer_id: Cow::Borrowed(peer_id),
            }),
        }
    }

    /// Returns the list of all the peers we know of.
    /// todo sample sorting
    pub fn sample_peers(&self, room_id: &RoomId) -> impl Iterator<Item = PeerId> {
        assert!(self.rooms.contains_key(&room_id));

        Ok(self
            .nodes
            .iter()
            .filter(move |(_, n)| n.rooms.contains_key(room_id))
            .sorted_by_key(|(p, _)| p.to_bytes())
            .map(|(p, _)| p.clone()))
    }

    /// Returns the index of a specified peer in a given set.
    pub fn index_of(&self, room_id: &RoomId, peer: PeerId) -> Option<usize> {
        assert!(self.rooms.contains_key(&room_id));

        self.nodes
            .iter()
            .filter(move |(_, n)| n.member_of_and(room_id, |ms| ms.is_connected()))
            .sorted_by_key(|(p, _)| p.to_bytes())
            .position(|elem| *elem == peer)
    }

    /// Returns the index of a specified peer in a given set.
    pub fn at_index(&self, room_id: &RoomId, index: usize) -> Option<PeerId> {
        assert!(self.rooms.contains_key(&room_id));

        self.nodes
            .iter()
            .filter(move |(_, n)| n.member_of_and(room_id, |ms| ms.is_connected()))
            .map(|(p, _)| p)
            .sorted_by_key(|p| p.to_bytes())
            .enumerate()
            .find_map(move |(i, p)| if i == index { Some(p.clone()) } else { None })
    }

    /// Returns the list of peers we are connected to in the context of the set.
    pub fn connected_peers(&self, room_id: &RoomId) -> impl Iterator<Item = &PeerId> {
        assert!(self.rooms.contains_key(&room_id));

        self.nodes
            .iter()
            .filter(move |(_, n)| n.member_of_and(room_id, |ms| ms.is_connected()))
            .map(|(p, _)| p)
    }
}

/// State of a single node that we know about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Node {
    /// Map of sets the node to session_ids.
    pub(crate) rooms: HashMap<RoomId, MembershipState>,
}

impl Node {
    pub(crate) fn new() -> Self {
        Self {
            rooms: (0..num_sets).map(|_| MembershipState::NotMember).collect(),
        }
    }

    fn member_of_and<P>(&self, room_id: &RoomId, predicate: P) -> bool
    where
        P: FnOnce(&MembershipState) -> bool,
    {
        self.rooms.contains_key(room_id) && self.rooms.get(room_id).map(predicate).unwrap()
    }
}
