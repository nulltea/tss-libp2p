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

//! Peer Set Manager (PSM). Contains the strategy for choosing which nodes the network should be
//! connected to.
//!
//! The PSM handles *sets* of nodes. A set of nodes is defined as the nodes that are believed to
//! support a certain capability, such as handling blocks and transactions of a specific chain,
//! or collating a certain parachain.
//!
//! For each node in each set, the peerset holds a flag specifying whether the node is
//! connected to us or not.
//!
//! This connected/disconnected status is specific to the node and set combination, and it is for
//! example possible for a node to be connected through a specific set but not another.
//!
//! In addition, for each, set, the peerset also holds a list of reserved nodes towards which it
//! will at all time try to maintain a connection with.

pub mod peers_state;

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

pub use crate::peers_state::SetConfig;

/// We don't accept nodes whose reputation is under this value.
pub const BANNED_THRESHOLD: i32 = 82 * (i32::MIN / 100);
/// Reputation change for a node when we get disconnected from it.
const DISCONNECT_REPUTATION_CHANGE: i32 = -256;
/// Amount of time between the moment we disconnect from a node and the moment we remove it from
/// the list.
const FORGET_AFTER: Duration = Duration::from_secs(3600);

#[derive(Debug)]
enum Action {
    SetSetAsStatic(SetId, bool),
    ReportPeer(PeerId, ReputationChange),
    AddToPeersSet(SetId, PeerId),
    RemoveFromPeersSet(SetId, PeerId),
    PeerReputation(PeerId, oneshot::Sender<i32>),
}

/// Identifier of a set in the peerset.
///
/// Can be constructed using the `From<usize>` trait implementation based on the index of the set
/// within [`PeersetConfig::sets`]. For example, the first element of [`PeersetConfig::sets`] is
/// later referred to with `SetId::from(0)`. It is intended that the code responsible for building
/// the [`PeersetConfig`] is also responsible for constructing the [`SetId`]s.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SetId(pub usize);

impl SetId {
    pub const fn from(id: usize) -> Self {
        Self(id)
    }
}

impl From<usize> for SetId {
    fn from(id: usize) -> Self {
        Self(id)
    }
}

impl From<SetId> for usize {
    fn from(id: SetId) -> Self {
        id.0
    }
}

/// Description of a reputation adjustment for a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReputationChange {
    /// Reputation delta.
    pub value: i32,
    /// Reason for reputation change.
    pub reason: &'static str,
}

impl ReputationChange {
    /// New reputation change with given delta and reason.
    pub const fn new(value: i32, reason: &'static str) -> ReputationChange {
        Self { value, reason }
    }

    /// New reputation change that forces minimum possible reputation.
    pub const fn new_fatal(reason: &'static str) -> ReputationChange {
        Self {
            value: i32::MIN,
            reason,
        }
    }
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

    /// Add a peer to a set.
    pub fn add_to_peers_set(&self, set_id: SetId, peer_id: PeerId) {
        let _ = self
            .tx
            .unbounded_send(Action::AddToPeersSet(set_id, peer_id));
    }

    /// Remove a peer from a set.
    pub fn remove_from_peers_set(&self, set_id: SetId, peer_id: PeerId) {
        let _ = self
            .tx
            .unbounded_send(Action::RemoveFromPeersSet(set_id, peer_id));
    }

    /// Returns the reputation value of the peer.
    pub async fn peer_reputation(self, peer_id: PeerId) -> Result<i32, ()> {
        let (tx, rx) = oneshot::channel();

        let _ = self.tx.unbounded_send(Action::PeerReputation(peer_id, tx));

        // The channel can only be closed if the peerset no longer exists.
        rx.await.map_err(|_| ())
    }
}

/// Message that can be sent by the peer set manager (PSM).
#[derive(Debug, PartialEq)]
pub enum Message {
    /// Request to open a connection to the given peer. From the point of view of the PSM, we are
    /// immediately connected.
    Connect {
        set_id: SetId,
        /// Peer to connect to.
        peer_id: PeerId,
    },

    /// Drop the connection to the given peer, or cancel the connection attempt after a `Connect`.
    Drop {
        set_id: SetId,
        /// Peer to disconnect from.
        peer_id: PeerId,
    },

    /// Equivalent to `Connect` for the peer corresponding to this incoming index.
    Accept(IncomingIndex),

    /// Equivalent to `Drop` for the peer corresponding to this incoming index.
    Reject(IncomingIndex),
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
#[derive(Debug)]
pub struct PeersetConfig {
    /// List of sets of nodes the peerset manages.
    pub sets: Vec<SetConfig>,
}

/// Side of the peer set manager owned by the network. In other words, the "receiving" side.
///
/// Implements the `Stream` trait and can be polled for messages. The `Stream` never ends and never
/// errors.
#[derive(Debug)]
pub struct Peerset {
    /// Underlying data structure for the nodes's states.
    data: peers_state::PeersState,

    /// Receiver for messages from the `PeersetHandle` and from `tx`.
    rx: mpsc::UnboundedReceiver<Action>,
    /// Sending side of `rx`.
    tx: mpsc::UnboundedSender<Action>,
    /// Queue of messages to be emitted when the `Peerset` is polled.
    message_queue: VecDeque<Message>,
    /// When the `Peerset` was created.
    created: Instant,
    /// Last time when we updated the reputations of connected nodes.
    latest_time_update: Instant,
}

impl Peerset {
    /// Builds a new peerset from the given configuration.
    pub fn from_config(config: PeersetConfig) -> (Self, PeersetHandle) {
        let (tx, rx) = mpsc::unbounded();

        let handle = PeersetHandle { tx: tx.clone() };

        let mut peerset = {
            let now = Instant::now();

            Self {
                data: peers_state::PeersState::new(config.sets.iter().map(|set| SetConfig {
                    peers_limit: set.peers_limit,
                    initial_nodes: set.initial_nodes.clone(),
                    static_set: set.static_set,
                    boot_nodes: None,
                })),
                tx,
                rx,
                message_queue: VecDeque::new(),
                created: now,
                latest_time_update: now,
            }
        };

        for (set, set_config) in config.sets.into_iter().enumerate() {
            for node in set_config.initial_nodes {
                peerset.add_to_peers_set(SetId::from(set), node);
            }

            if let Some(boot_nodes) = set_config.boot_nodes {
                for peer_id in boot_nodes {
                    if let peers_state::Peer::Unknown(entry) = peerset.data.peer(set, &peer_id) {
                        entry.discover();
                    } else {
                        debug!(target: "peerset", "Duplicate bootstrap node in config: {:?}", peer_id);
                    }
                }
            }
        }

        (peerset, handle)
    }

    /// Returns copy of the peerset handle.
    pub fn get_handle(&self) -> PeersetHandle {
        PeersetHandle {
            tx: self.tx.clone(),
        }
    }

    /// Returns the list of connected peers.
    pub fn connected_peers(&self, set_id: SetId) -> impl Iterator<Item = &PeerId> {
        self.data.connected_peers(set_id.0)
    }

    /// Returns the index of a specified peer in a given set.
    pub fn index_of(&self, set_id: SetId, peer: PeerId) -> Option<usize> {
        self.data
            .connected_peers(set_id.0)
            .position(|elem| *elem == peer)
    }

    /// Returns the index of a specified peer in a given set.
    pub fn at_index(&self, set_id: SetId, index: usize) -> Option<&PeerId> {
        let peers: Vec<&PeerId> = self.data.connected_peers(set_id.0).collect();

        if peers.len() > index {
            Some(peers[index])
        } else {
            None
        }
    }

    /// Adds a node to the given set. The peerset will, if possible and not already the case,
    /// try to connect to it.
    ///
    /// > **Note**: This has the same effect as [`PeersetHandle::add_to_peers_set`].
    pub fn add_to_peers_set(&mut self, set_id: SetId, peer_id: PeerId) {
        if let peers_state::Peer::Unknown(entry) = self.data.peer(set_id.0, &peer_id) {
            entry.discover();
        }
    }

    fn on_remove_from_peers_set(&mut self, set_id: SetId, peer_id: PeerId) {
        match self.data.peer(set_id.0, &peer_id) {
            peers_state::Peer::Connected(peer) => {
                self.message_queue.push_back(Message::Drop {
                    set_id,
                    peer_id: *peer.peer_id(),
                });
                peer.disconnect().forget_peer();
            }
            peers_state::Peer::NotConnected(peer) => {
                peer.forget_peer();
            }
            peers_state::Peer::Unknown(_) => {}
        }
    }

    fn on_report_peer(&mut self, peer_id: PeerId, change: ReputationChange) {
        // We want reputations to be up-to-date before adjusting them.
        self.update_time();

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

        for set_index in 0..self.data.num_sets() {
            if let peers_state::Peer::Connected(peer) = self.data.peer(set_index, &peer_id) {
                let peer = peer.disconnect();
                self.message_queue.push_back(Message::Drop {
                    set_id: SetId(set_index),
                    peer_id: peer.into_peer_id(),
                });
            }
        }
    }

    fn on_peer_reputation(&mut self, peer_id: PeerId, pending_response: oneshot::Sender<i32>) {
        let reputation = self.data.peer_reputation(peer_id);
        let _ = pending_response.send(reputation.reputation());
    }

    /// Updates the value of `self.latest_time_update` and performs all the updates that happen
    /// over time, such as reputation increases for staying connected.
    fn update_time(&mut self) {
        let now = Instant::now();

        // We basically do `(now - self.latest_update).as_secs()`, except that by the way we do it
        // we know that we're not going to miss seconds because of rounding to integers.
        let secs_diff = {
            let elapsed_latest = self.latest_time_update - self.created;
            let elapsed_now = now - self.created;
            self.latest_time_update = now;
            elapsed_now.as_secs() - elapsed_latest.as_secs()
        };

        // For each elapsed second, move the node reputation towards zero.
        // If we multiply each second the reputation by `k` (where `k` is between 0 and 1), it
        // takes `ln(0.5) / ln(k)` seconds to reduce the reputation by half. Use this formula to
        // empirically determine a value of `k` that looks correct.
        for _ in 0..secs_diff {
            for peer_id in self.data.peers().cloned().collect::<Vec<_>>() {
                // We use `k = 0.98`, so we divide by `50`. With that value, it takes 34.3 seconds
                // to reduce the reputation by half.
                fn reput_tick(reput: i32) -> i32 {
                    let mut diff = reput / 50;
                    if diff == 0 && reput < 0 {
                        diff = -1;
                    } else if diff == 0 && reput > 0 {
                        diff = 1;
                    }
                    reput.saturating_sub(diff)
                }

                let mut peer_reputation = self.data.peer_reputation(peer_id);

                let before = peer_reputation.reputation();
                let after = reput_tick(before);
                trace!(target: "peerset", "Fleeting {}: {} -> {}", peer_id, before, after);
                peer_reputation.set_reputation(after);

                if after != 0 {
                    continue;
                }

                drop(peer_reputation);

                // If the peer reaches a reputation of 0, and there is no connection to it,
                // forget it.
                for set_index in 0..self.data.num_sets() {
                    match self.data.peer(set_index, &peer_id) {
                        peers_state::Peer::Connected(_) => {}
                        peers_state::Peer::NotConnected(peer) => {
                            if peer.last_connected_or_discovered() + FORGET_AFTER < now {
                                peer.forget_peer();
                            }
                        }
                        peers_state::Peer::Unknown(_) => {
                            // Happens if this peer does not belong to this set.
                        }
                    }
                }
            }
        }
    }

    /// Indicate that we received an incoming connection. Must be answered either with
    /// a corresponding `Accept` or `Reject`, except if we were already connected to this peer.
    ///
    /// Note that this mechanism is orthogonal to `Connect`/`Drop`. Accepting an incoming
    /// connection implicitly means `Connect`, but incoming connections aren't cancelled by
    /// `dropped`.
    // Implementation note: because of concurrency issues, it is possible that we push a `Connect`
    // message to the output channel with a `PeerId`, and that `incoming` gets called with the same
    // `PeerId` before that message has been read by the user. In this situation we must not answer.
    pub fn incoming(&mut self, set_id: SetId, peer_id: PeerId, index: IncomingIndex) {
        trace!(target: "peerset", "Incoming {:?}", peer_id);

        self.update_time();

        let not_connected = match self.data.peer(set_id.0, &peer_id) {
            // If we're already connected, don't answer, as the docs mention.
            peers_state::Peer::Connected(_) => return,
            peers_state::Peer::NotConnected(mut entry) => {
                entry.bump_last_connected_or_discovered();
                entry
            }
            peers_state::Peer::Unknown(entry) => entry.discover(),
        };

        if not_connected.reputation() < BANNED_THRESHOLD {
            self.message_queue.push_back(Message::Reject(index));
            return;
        }

        match not_connected.try_accept_peer() {
            Ok(_) => self.message_queue.push_back(Message::Accept(index)),
            Err(_) => self.message_queue.push_back(Message::Reject(index)),
        }
    }

    /// Indicate that we dropped an active connection with a peer, or that we failed to connect.
    ///
    /// Must only be called after the PSM has either generated a `Connect` message with this
    /// `PeerId`, or accepted an incoming connection with this `PeerId`.
    pub fn dropped(&mut self, set_id: SetId, peer_id: PeerId, reason: DropReason) {
        // We want reputations to be up-to-date before adjusting them.
        self.update_time();

        match self.data.peer(set_id.0, &peer_id) {
            peers_state::Peer::Connected(mut entry) => {
                // Decrease the node's reputation so that we don't try it again and again and again.
                entry.add_reputation(DISCONNECT_REPUTATION_CHANGE);
                trace!(target: "peerset", "Dropping {}: {:+} to {}",
					peer_id, DISCONNECT_REPUTATION_CHANGE, entry.reputation());
                entry.disconnect();
            }
            peers_state::Peer::NotConnected(_) | peers_state::Peer::Unknown(_) => {
                error!(target: "peerset", "Received dropped() for non-connected node")
            }
        }

        if let DropReason::Refused = reason {
            self.on_remove_from_peers_set(set_id, peer_id);
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

    /// Produces a JSON object containing the state of the peerset manager, for debugging purposes.
    pub fn debug_info(&mut self) -> serde_json::Value {
        self.update_time();

        json!({
            "sets": (0..self.data.num_sets()).map(|set_index| {
                json!({
                    "nodes": self.data.peers().cloned().collect::<Vec<_>>().into_iter().filter_map(|peer_id| {
                        let state = match self.data.peer(set_index, &peer_id) {
                            peers_state::Peer::Connected(entry) => json!({
                                "connected": true,
                                "reputation": entry.reputation()
                            }),
                            peers_state::Peer::NotConnected(entry) => json!({
                                "connected": false,
                                "reputation": entry.reputation()
                            }),
                            peers_state::Peer::Unknown(_) => return None,
                        };

                        Some((peer_id.to_base58(), state))
                    }).collect::<HashMap<_, _>>()
                })
            }).collect::<Vec<_>>(),
            "message_queue": self.message_queue.len(),
        })
    }

    /// Returns the number of peers that we have discovered.
    pub fn num_discovered_peers(&self) -> usize {
        self.data.peers().len()
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
                Action::SetSetAsStatic(set_id, reserved) => {}
                // TODO: self.lock_set(set_id)
                Action::ReportPeer(peer_id, score_diff) => self.on_report_peer(peer_id, score_diff),
                Action::AddToPeersSet(sets_name, peer_id) => {
                    self.add_to_peers_set(sets_name, peer_id)
                }
                Action::RemoveFromPeersSet(sets_name, peer_id) => {
                    self.on_remove_from_peers_set(sets_name, peer_id)
                }
                Action::PeerReputation(peer_id, pending_response) => {
                    self.on_peer_reputation(peer_id, pending_response)
                }
            }
        }
    }
}

/// Reason for calling [`Peerset::dropped`].
pub enum DropReason {
    /// Substream or connection has been closed for an unknown reason.
    Unknown,
    /// Substream or connection has been explicitly refused by the target. In other words, the
    /// peer doesn't actually belong to this set.
    ///
    /// This has the side effect of calling [`PeersetHandle::remove_from_peers_set`].
    Refused,
}
