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
use libp2p::PeerId;
use log::{debug, error, trace};
use serde_json::json;
use std::{
    collections::{HashMap, VecDeque},
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, Instant},
};
use std::collections::HashSet;

pub use crate::peer::*;

/// We don't accept nodes whose reputation is under this value.
pub const BANNED_THRESHOLD: i32 = 82 * (i32::MIN / 100);
/// Reputation change for a node when we get disconnected from it.
const DISCONNECT_REPUTATION_CHANGE: i32 = -256;
/// Amount of time between the moment we disconnect from a node and the moment we remove it from
/// the list.
const FORGET_AFTER: Duration = Duration::from_secs(3600);

#[derive(Debug)]
enum Action {
    AddToPeersSet(PeerId),
    RemoveFromPeersSet(PeerId),
    GetPeerIndex(PeerId, oneshot::Sender<u16>),
    GetPeerAtIndex(u16, oneshot::Sender<PeerId>),
    ReportPeer(PeerId, ReputationChange),
    GetPeerReputation(PeerId, oneshot::Sender<(u16, i32)>),
}

/// Shared handle to the peer set manager (PSM). Distributed around the code.
#[derive(Debug, Clone)]
pub struct PeersetHandle {
    tx: mpsc::UnboundedSender<Action>,
}

impl PeersetHandle {
    /// Reports an adjustment to the reputation of the given peer.
    pub fn report_peer(&self, peer_id: PeerId, score_diff: ReputationChange) {
        let _ = self
            .tx
            .unbounded_send(Action::ReportPeer(peer_id, score_diff));
    }

    /// Add a peer to the set.
    pub fn add_to_peers_set(&self, peer_id: PeerId) {
        let _ = self
            .tx
            .unbounded_send(Action::AddToPeersSet(peer_id));
    }

    /// Remove a peer from the set.
    pub fn remove_from_peers_set(&self, peer_id: PeerId) {
        let _ = self
            .tx
            .unbounded_send(Action::RemoveFromPeersSet(peer_id));
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

    /// Returns the reputation value of the peer.
    pub async fn peer_reputation(self, peer_id: PeerId) -> Result<(u16, i32), ()> {
        let (tx, rx) = oneshot::channel();

        let _ = self.tx.unbounded_send(Action::GetPeerReputation(peer_id, tx));

        // The channel can only be closed if the peerset no longer exists.
        rx.await.map_err(|_| ())
    }
}

/// Message that can be sent by the peer set manager (PSM).
#[derive(Debug, PartialEq)]
pub enum Message {
    /// Request to open a connection to the given peer. From the point of view of the PSM, we are
    /// immediately connected.
    Connect(PeerId),

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

    fn on_report_peer(&mut self, peer_id: PeerId, change: ReputationChange) {
        let mut reputation = self.data.peer_reputation(peer_id);
        reputation.add_reputation(change.value);
        if reputation.reputation() >= BANNED_THRESHOLD {
            trace!(target: "peerset", "Report {}: {:+} to {}. Reason: {}",
                peer_id, change.value, reputation.reputation(), change.reason
            );
            return;
        }

        debug!(target: "peerset", "Report {}: {:+} to {}. Reason: {}, Disconnecting",
            peer_id, change.value, reputation.reputation(), change.reason
        );

        drop(reputation);

        if let Peer::Connected(peer) = self.data.peer(&peer_id) {
            let peer = peer.disconnect();
            self.message_queue.push_back(Message::Drop(peer.into_peer_id()));
        }
    }

    fn get_peer_reputation(&mut self, peer_id: PeerId, pending_result: oneshot::Sender<(u16, i32)>) {
        if let Some(index) = self.data.index_of(peer_id) {
            let reputation = self.data.peer_reputation(peer_id);
            let _ = pending_result.send((index as u16, reputation.reputation()));
        } else {
            drop(pending_result)
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

    /// Reports an adjustment to the reputation of the given peer.
    pub fn report_peer(&mut self, peer_id: PeerId, score_diff: ReputationChange) {
        // We don't immediately perform the adjustments in order to have state consistency. We
        // don't want the reporting here to take priority over messages sent using the
        // `PeersetHandle`.
        let _ = self
            .tx
            .unbounded_send(Action::ReportPeer(peer_id, score_diff));
    }

    /// Returns the number of peers that we have discovered.
    pub fn num_discovered_peers(&self) -> usize {
        self.data.peer_ids().len()
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
                Action::AddToPeersSet(peer_id) => {
                    self.add_to_peers_set(peer_id)
                }
                Action::RemoveFromPeersSet(peer_id) => {
                    self.on_remove_from_peers_set(peer_id)
                }
                Action::GetPeerIndex(peer_id, pending_result) => {
                    self.get_peer_index(peer_id, pending_result)
                }
                Action::GetPeerAtIndex(index, pending_result) => {
                    self.get_peer_id(index, pending_result)
                }
                Action::ReportPeer(peer_id, score_diff) => {
                    self.on_report_peer(peer_id, score_diff)
                }
                Action::GetPeerReputation(peer_id, pending_result) => {
                    self.get_peer_reputation(peer_id, pending_result)
                }
            }
        }
    }
}
