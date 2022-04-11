// This file is part of Substrate.

// Copyright (C) 2018-2021 Parity Technologies (UK) Ltd.
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

mod peer;
mod state;

use futures::{channel::mpsc, channel::oneshot, prelude::*};
use libp2p::{Multiaddr, PeerId};
use log::debug;
use std::collections::HashSet;
use std::{
    collections::VecDeque,
    pin::Pin,
    task::{Context, Poll},
    time::Instant,
};

pub use crate::peer::*;

#[derive(Debug)]
enum Action {
    AddToPeersSet(PeerId),
    RemoveFromPeersSet(PeerId),
    GetPeerIndex(PeerId, oneshot::Sender<u16>),
    GetPeerAtIndex(u16, oneshot::Sender<PeerId>),
    GetPeerIds(oneshot::Sender<Vec<PeerId>>),
}

/// Shared handle to the peer set manager (PSM). Distributed around the code.
#[derive(Debug, Clone)]
pub struct PeersetHandle {
    tx: mpsc::UnboundedSender<Action>,
}

impl PeersetHandle {
    /// Add a peer to the set.
    pub fn add_to_peers_set(&self, peer_id: PeerId) {
        let _ = self.tx.unbounded_send(Action::AddToPeersSet(peer_id));
    }

    /// Remove a peer from the set.
    pub fn remove_from_peers_set(&self, peer_id: PeerId) {
        let _ = self.tx.unbounded_send(Action::RemoveFromPeersSet(peer_id));
    }

    /// Returns the index of the peer.
    pub async fn peer_index(self, peer_id: PeerId) -> Result<u16, ()> {
        let (tx, rx) = oneshot::channel();

        let _ = self.tx.unbounded_send(Action::GetPeerIndex(peer_id, tx));

        // The channel is closed only if sender refuses sending index,
        // due to that peer not being a part of the set.
        rx.await.map_err(|_| ())
    }

    /// Returns the index of the peer.
    pub async fn peer_at_index(self, index: u16) -> Result<PeerId, ()> {
        let (tx, rx) = oneshot::channel();

        let _ = self.tx.unbounded_send(Action::GetPeerAtIndex(index, tx));

        // The channel is closed only if sender refuses sending index,
        // due to that peer not being a part of the set.
        rx.await.map_err(|_| ())
    }

    /// Returns the ids of all peers in the set.
    pub async fn peer_ids(self) -> Result<impl ExactSizeIterator<Item = PeerId>, ()> {
        let (tx, rx) = oneshot::channel();

        let _ = self.tx.unbounded_send(Action::GetPeerIds(tx));

        rx.await.map_err(|_| ()).map(|r| r.into_iter())
    }
}

/// Message that can be sent by the peer set manager (PSM).
#[derive(Debug, PartialEq)]
pub enum Message {
    /// Request to open a connection to the given peer. From the point of view of the PSM, we are
    /// immediately connected.
    Connect(Multiaddr),

    /// Drop the connection to the given peer, or cancel the connection attempt after a `Connect`.
    Drop(PeerId),
}

/// Opaque identifier for an incoming connection. Allocated by the network.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct IncomingIndex(pub u64);

impl From<u64> for IncomingIndex {
    fn from(val: u64) -> Self {
        Self(val)
    }
}

/// Configuration to pass when creating the peer set manager.
#[derive(Debug, Clone)]
pub struct PeersetConfig {
    /// Maximum number of ingoing links to peers.
    /// Zero value means unlimited.
    pub peers_limit: u32,

    /// List of bootstrap nodes to initialize the set with.
    ///
    /// > **Note**: Keep in mind that the networking has to know an address for these nodes,
    /// >           otherwise it will not be able to connect to them.
    pub boot_nodes: Option<Vec<PeerId>>,

    /// Lists of nodes we should always be connected to.
    ///
    /// > **Note**: Keep in mind that the networking has to know an address for these nodes,
    /// >			otherwise it will not be able to connect to them.
    pub initial_nodes: HashSet<PeerId>,

    /// If true, we only accept nodes in [`SetConfig::initial_nodes`].
    pub static_set: bool,
}

impl PeersetConfig {
    pub fn new_static(peers: impl Iterator<Item = PeerId>) -> Self {
        let mut set = Self {
            static_set: true,
            initial_nodes: peers.collect(),
            peers_limit: 0,
            boot_nodes: None,
        };

        set.peers_limit = set.initial_nodes.len() as u32;
        set
    }
}

/// State of a single set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SetInfo {
    /// Number of slot-occupying nodes for which the `MembershipState` is `In`.
    pub num_peers: u32,

    /// Maximum allowed number of slot-occupying nodes for which the `MembershipState` is `In`.
    pub max_peers: u32,

    /// List of node identities (discovered or not) that was introduced into the set
    /// via static configuration [`SetConfig::initial_nodes`].
    pub initial_nodes: HashSet<PeerId>,

    // States that peerset is locked and won't accept any more peers and changes.
    pub static_set: bool,
}

/// Side of the peer set manager owned by the network. In other words, the "receiving" side.
///
/// Implements the `Stream` trait and can be polled for messages. The `Stream` never ends and never
/// errors.
#[derive(Debug)]
pub struct Peerset {
    /// Underlying data structure for the nodes's states.
    data: state::PeersState,

    /// Receiver for messages from the `PeersetHandle` and from `tx`.
    rx: mpsc::UnboundedReceiver<Action>,
    /// Sending side of `rx`.
    tx: mpsc::UnboundedSender<Action>,
    /// Queue of messages to be emitted when the `Peerset` is polled.
    message_queue: VecDeque<Message>,
    /// When the `Peerset` was created.
    created: Instant,
}

impl Peerset {
    /// Builds a new peerset from the given configuration.
    pub fn from_config(local_peer_id: PeerId, config: PeersetConfig) -> (Self, PeersetHandle) {
        let (tx, rx) = mpsc::unbounded();

        let handle = PeersetHandle { tx: tx.clone() };

        let mut peerset = {
            let now = Instant::now();

            Self {
                data: state::PeersState::new(local_peer_id, config.clone()),
                tx,
                rx,
                message_queue: VecDeque::new(),
                created: now,
            }
        };

        for peer_id in config.initial_nodes {
            peerset.add_to_peers_set(peer_id);
        }

        if let Some(boot_nodes) = config.boot_nodes {
            for peer_id in boot_nodes {
                if let Peer::Unknown(entry) = peerset.data.peer(&peer_id) {
                    entry.discover();
                } else {
                    debug!(target: "peerset", "Duplicate bootstrap node in config: {:?}", peer_id);
                }
            }
        }

        (peerset, handle)
    }

    pub fn state(&self) -> &state::PeersState {
        &self.data
    }

    pub fn state_mut(&mut self) -> &mut state::PeersState {
        &mut self.data
    }

    /// Returns copy of the peerset handle.
    pub fn get_handle(&self) -> PeersetHandle {
        PeersetHandle {
            tx: self.tx.clone(),
        }
    }

    /// Adds a node to the given set. The peerset will, if possible and not already the case,
    /// try to connect to it.
    ///
    /// > **Note**: This has the same effect as [`PeersetHandle::add_to_peers_set`].
    pub fn add_to_peers_set(&mut self, peer_id: PeerId) {
        if let Peer::Unknown(entry) = self.data.peer(&peer_id) {
            entry.discover();
        }
    }

    fn on_remove_from_peers_set(&mut self, peer_id: PeerId) {
        match self.data.peer(&peer_id) {
            Peer::Connected(peer) => {
                self.message_queue.push_back(Message::Drop(*peer.peer_id()));
                peer.disconnect().forget_peer();
            }
            Peer::NotConnected(peer) => {
                peer.forget_peer();
            }
            Peer::Unknown(_) => {}
        }
    }

    fn get_peer_index(&mut self, peer_id: PeerId, pending_result: oneshot::Sender<u16>) {
        if let Some(index) = self.data.index_of(peer_id) {
            let _ = pending_result.send(index as u16);
        } else {
            drop(pending_result)
        }
    }

    fn get_peer_id(&mut self, index: u16, pending_result: oneshot::Sender<PeerId>) {
        if let Some(peer) = self.data.at_index(index as usize) {
            let _ = pending_result.send(peer);
        } else {
            drop(pending_result)
        }
    }

    /// Returns the number of peers that we have discovered.
    pub fn num_discovered_peers(&self) -> usize {
        self.data.nodes.len()
    }
}

impl Stream for Peerset {
    type Item = Message;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        loop {
            if let Some(message) = self.message_queue.pop_front() {
                return Poll::Ready(Some(message));
            }

            let action = match Stream::poll_next(Pin::new(&mut self.rx), cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(event)) => event,
                Poll::Ready(None) => return Poll::Pending,
            };

            match action {
                Action::AddToPeersSet(peer_id) => self.add_to_peers_set(peer_id),
                Action::RemoveFromPeersSet(peer_id) => self.on_remove_from_peers_set(peer_id),
                Action::GetPeerIndex(peer_id, pending_result) => {
                    self.get_peer_index(peer_id, pending_result)
                }
                Action::GetPeerAtIndex(index, pending_result) => {
                    self.get_peer_id(index, pending_result)
                }
                Action::GetPeerIds(pending_result) => {
                    pending_result.send(self.data.peer_ids().collect());
                }
            }
        }
    }
}
