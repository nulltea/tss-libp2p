// This file was a part of Substrate.
mod ids;
mod peer;
mod state;

use futures::{channel::mpsc, channel::oneshot, prelude::*};
use libp2p::{Multiaddr, PeerId};
use log::debug;
use std::borrow::Cow;
use std::collections::hash_map::Entry;
use std::collections::HashSet;
use std::{
    collections::VecDeque,
    pin::Pin,
    task::{Context, Poll},
    time::Instant,
};

pub use crate::ids::*;
pub use crate::peer::*;
use crate::state::Node;

#[derive(Debug)]
enum Action {
    AddToPeersSet(RoomId, PeerId),
    RemoveFromPeersSet(RoomId, PeerId),

    GetPeerIndex(RoomId, PeerId, oneshot::Sender<u16>),
    GetPeerAtIndex(RoomId, u16, oneshot::Sender<PeerId>),
    GetPeers(RoomId, oneshot::Sender<Vec<(PeerId, MembershipState)>>),
}

/// Shared handle to the peer set manager (PSM). Distributed around the code.
#[derive(Debug, Clone)]
pub struct PeersetHandle {
    tx: mpsc::UnboundedSender<Action>,
}

impl PeersetHandle {
    /// Add a peer to the set.
    pub fn add_to_peers_set(&self, room_id: &RoomId, peer_id: PeerId) {
        let _ = self
            .tx
            .unbounded_send(Action::AddToPeersSet(room_id.clone(), peer_id));
    }

    /// Remove a peer from the set.
    pub fn remove_from_peers_set(&self, room_id: &RoomId, peer_id: PeerId) {
        let _ = self
            .tx
            .unbounded_send(Action::RemoveFromPeersSet(room_id.clone(), peer_id));
    }

    /// Returns the index of the peer.
    pub async fn index_of_peer(self, session_id: SessionId, peer_id: PeerId) -> Result<u16, ()> {
        let (tx, rx) = oneshot::channel();

        let _ = self
            .tx
            .unbounded_send(Action::GetPeerIndex(session_id, peer_id, tx));

        // The channel is closed only if sender refuses sending index,
        // due to that peer not being a part of the set.
        rx.await.map_err(|_| ())
    }

    /// Returns the index of the peer.
    pub async fn peer_at_index(self, session_id: SessionId, index: u16) -> Result<PeerId, ()> {
        let (tx, rx) = oneshot::channel();

        let _ = self
            .tx
            .unbounded_send(Action::GetPeerAtIndex(session_id, index, tx));

        // The channel is closed only if sender refuses sending index,
        // due to that peer not being a part of the set.
        rx.await.map_err(|_| ())
    }

    /// Returns the index of the peer.
    pub async fn peers(self, room_id: RoomId) -> Result<Vec<(PeerId, MembershipState)>, ()> {
        let (tx, rx) = oneshot::channel();

        let _ = self.tx.unbounded_send(Action::GetPeers(room_id, tx));

        // The channel is closed only if sender refuses sending index,
        // due to that peer not being a part of the set.
        rx.await.map_err(|_| ())
    }
}

/// Message that can be sent by the peer set manager (PSM).
#[derive(Debug, PartialEq)]
pub enum Message {
    /// Request to open a connection to the given peer. From the point of view of the PSM, we are
    /// immediately connected.
    Connect {
        peer_id: PeerId,
        room_id: RoomId,
        session_id: SessionId,
        ack: mpsc::Sender<Result<Vec<u8>, ()>>,
    },

    /// Drop the connection to the given peer, or cancel the connection attempt after a `Connect`.
    Drop { room_id: RoomId, peer_id: PeerId },
}

/// Configuration to pass when creating the peer set manager.
#[derive(Debug)]
pub struct PeersetConfig {
    /// List of sets of nodes the peerset manages.
    pub sets: Vec<SetConfig>,
}

/// Configuration for a single set of nodes.
#[derive(Debug, Clone)]
pub struct SetConfig {
    /// Maximum number of ingoing links to peers.
    /// Zero value means unlimited.
    pub max_size: u32,

    /// List of bootstrap nodes to initialize the set with.
    ///
    /// > **Note**: Keep in mind that the networking has to know an address for these nodes,
    /// >           otherwise it will not be able to connect to them.
    pub boot_nodes: Vec<PeerId>,
}

impl SetConfig {
    pub fn new(peers: impl Iterator<Item = PeerId>, max_size: u32) -> Self {
        Self {
            max_size,
            boot_nodes: peers.collect(),
        }
    }
}

/// State of a single set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SetInfo {
    /// Number of nodes for which the `MembershipState` is `Connected` or `NotConnected`.
    pub num_peers: u32,

    /// Maximum allowed number of slot-occupying nodes for which the `MembershipState` is `Connected`.
    pub max_peers: u32,

    /// List of node identities (discovered or not) that was introduced into the set
    /// via static configuration [`SetConfig::initial_nodes`].
    pub initial_nodes: HashSet<PeerId>,
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
    pub fn from_config(config: PeersetConfig) -> (Self, PeersetHandle) {
        let (tx, rx) = mpsc::unbounded();

        let handle = PeersetHandle { tx: tx.clone() };

        let mut peerset = {
            let now = Instant::now();

            Self {
                data: state::PeersState::new(config.sets),
                tx,
                rx,
                message_queue: VecDeque::new(),
                created: now,
            }
        };

        for (set, set_config) in config.sets.into_iter().enumerate() {
            for peer_id in set_config.boot_nodes {
                if let Peer::Unknown(entry) = peerset.data.peer(set, &peer_id) {
                    entry.discover();
                } else {
                    debug!(target: "peerset", "Duplicate boot node in config: {:?}", peer_id);
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

    pub fn connected_peers(&mut self, room_id: &RoomId) -> impl Iterator<Item = &PeerId> {
        self.data.connected_peers(room_id)
    }

    /// Adds a node to the given set. The peerset will, if possible and not already the case,
    /// try to connect to it.
    pub fn add_to_peers_set(&mut self, room_id: &RoomId, peer_id: &PeerId) {
        if let Peer::Unknown(entry) = self.data.peer(room_id, peer_id) {
            entry.discover();
        }
    }

    pub fn incoming_connection(&mut self, room_id: &RoomId, peer_id: &PeerId) {
        match self.data.peer(room_id, peer_id) {
            Peer::NotConnected(peer) => peer.try_accept_peer(),
            Peer::Connected(_) => {}
            Peer::Unknown(_) => {}
        };
    }

    pub fn closed_connection(&mut self, room_id: &RoomId, peer_id: &PeerId) {
        match self.data.peer(room_id, peer_id) {
            Peer::Connected(peer) => peer.disconnect(),
            Peer::NotConnected(_) => {}
            Peer::Unknown(_) => {}
        };
    }

    fn on_remove_from_peers_set(&mut self, room_id: &RoomId, peer_id: PeerId) {
        match self.data.peer(room_id.into(), &peer_id) {
            Peer::Connected(peer) => {
                self.message_queue.push_back(Message::Drop {
                    room_id: room_id.clone(),
                    peer_id: peer_id.clone(),
                });
                peer.disconnect().forget_peer();
            }
            Peer::NotConnected(peer) => {
                peer.forget_peer();
            }
            Peer::Unknown(_) => {}
        }
    }

    async fn get_known_peers(
        &mut self,
        room_id: &RoomId,
        pending_responses: mpsc::Sender<Vec<(PeerId, MembershipState)>>,
    ) {
        match self.data.sessions.entry(session_id) {
            Entry::Vacant(e) => {
                e.insert(room_id.clone());
            }
            _ => {}
        };

        for peer_id in self.data.sample_peers(room_id) {
            self.message_queue.push_back(Message::Connect {
                peer_id,
                room_id: room_id.clone(),
                session_id,
                ack: pending_responses.clone(),
            });
        }
    }

    fn get_peer_index(
        &mut self,
        session_id: SessionId,
        peer_id: PeerId,
        pending_result: oneshot::Sender<u16>,
    ) {
        let set_id = match self.data.sessions.get(&session_id) {
            Some(set_id) => *set_id,
            None => {
                drop(pending_result);
                return;
            }
        };

        if let Some(index) = self.data.index_of(set_id, peer_id) {
            let _ = pending_result.send(index as u16);
        } else {
            drop(pending_result)
        }
    }

    fn get_peer_id(
        &mut self,
        session_id: SessionId,
        index: u16,
        pending_result: oneshot::Sender<PeerId>,
    ) {
        let set_id = match self.data.sessions.get(&session_id) {
            Some(set_id) => *set_id,
            None => {
                drop(pending_result);
                return;
            }
        };

        if let Some(peer) = self.data.at_index(set_id, index as usize) {
            let _ = pending_result.send(peer);
        } else {
            drop(pending_result)
        }
    }

    /// Returns the number of peers that we have discovered.
    pub fn num_connected_peers(&self, set: usize) -> usize {
        self.data.rooms[set].num_peers as usize
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
                Action::AddToPeersSet(room_id, peer_id) => self.add_to_peers_set(set_id, &peer_id),
                Action::RemoveFromPeersSet(room_id, peer_id) => {
                    self.on_remove_from_peers_set(&room_id, peer_id)
                }
                Action::GetPeerIndex(session_id, peer_id, pending_result) => {
                    self.get_peer_index(session_id, peer_id, pending_result)
                }
                Action::GetPeerAtIndex(session_id, index, pending_result) => {
                    self.get_peer_id(session_id, index, pending_result)
                }
                Action::AllocateForSession(room_id, session_id, pending_responses) => {
                    self.get_known_peers(&room_id, session_id, pending_responses)
                }
            }
        }
    }
}
