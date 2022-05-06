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
    AddToPeersSet(SetId, PeerId),
    RemoveFromPeersSet(SetId, PeerId),

    RegisterSession(SessionId, SetId, SetSize, oneshot::Sender<()>),
    GetPeerIndex(SessionId, PeerId, oneshot::Sender<u16>),
    GetPeerAtIndex(SessionId, u16, oneshot::Sender<PeerId>),
    GetPeerIds(SessionId, oneshot::Sender<Vec<PeerId>>),
}

/// Shared handle to the peer set manager (PSM). Distributed around the code.
#[derive(Debug, Clone)]
pub struct PeersetHandle {
    tx: mpsc::UnboundedSender<Action>,
}

impl PeersetHandle {
    /// Add a peer to the set.
    pub fn add_to_peers_set(&self, set_id: SetId, peer_id: PeerId) {
        let _ = self
            .tx
            .unbounded_send(Action::AddToPeersSet(set_id, peer_id));
    }

    /// Remove a peer from the set.
    pub fn remove_from_peers_set(&self, set_id: SetId, peer_id: PeerId) {
        let _ = self
            .tx
            .unbounded_send(Action::RemoveFromPeersSet(set_id, peer_id));
    }

    pub async fn register_session(&self, session_id: SessionId, set_id: SetId, size: SetSize) {
        let (tx, rx) = oneshot::channel();

        let _ = self
            .tx
            .unbounded_send(Action::RegisterSession(session_id, set_id, size, tx));

        rx.await;
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

    /// Returns the ids of all peers in the set.
    pub async fn peer_ids(
        self,
        session_id: SessionId,
    ) -> Result<impl ExactSizeIterator<Item = PeerId>, ()> {
        let (tx, rx) = oneshot::channel();

        let _ = self.tx.unbounded_send(Action::GetPeerIds(session_id, tx));

        rx.await.map_err(|_| ()).map(|r| r.into_iter())
    }
}

/// Message that can be sent by the peer set manager (PSM).
#[derive(Debug, PartialEq)]
pub enum Message {
    /// Request to open a connection to the given peer. From the point of view of the PSM, we are
    /// immediately connected.
    Connect {
        set_id: SetId,
        peer_id: PeerId,
        addr: Multiaddr,
    },

    /// Drop the connection to the given peer, or cancel the connection attempt after a `Connect`.
    Drop { set_id: SetId, peer_id: PeerId },
    // GatherSet {
    //     protocol_id: Cow<'static, str>,
    //     session_id: SessionId,
    //     target_size: SetSize,
    //     on_ready: oneshot::Sender<()>,
    // },
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
    pub target_size: u32,

    /// List of bootstrap nodes to initialize the set with.
    ///
    /// > **Note**: Keep in mind that the networking has to know an address for these nodes,
    /// >           otherwise it will not be able to connect to them.
    pub boot_nodes: Vec<PeerId>,
}

impl SetConfig {
    pub fn new(peers: impl Iterator<Item = PeerId>, target_size: u32) -> Self {
        Self {
            target_size,
            boot_nodes: peers.collect(),
        }
    }
}

/// State of a single set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SetInfo {
    /// Number of slot-occupying nodes for which the `MembershipState` is `In`.
    pub num_peers: u32,

    /// Maximum allowed number of slot-occupying nodes for which the `MembershipState` is `In`.
    pub target_size: u32,

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

    pub fn num_sets(&self) -> usize {
        self.data.sets.len()
    }

    /// Adds a node to the given set. The peerset will, if possible and not already the case,
    /// try to connect to it.
    pub fn add_to_peers_set(&mut self, set_id: SetId, peer_id: PeerId) {
        if let Peer::Unknown(entry) = self.data.peer(set_id.into(), &peer_id) {
            entry.discover();
        }
    }

    pub fn incoming_connection(&mut self, set_id: SetId, peer_id: &PeerId) {
        match self.data.peer(set_id.into(), &peer_id) {
            Peer::NotConnected(peer) => peer.try_accept_peer(),
            Peer::Connected(_) => {}
            Peer::Unknown(_) => {}
        };
    }

    pub fn closed_connection(&mut self, set_id: SetId, peer_id: &PeerId) {
        match self.data.peer(set_id.into(), peer_id) {
            Peer::Connected(peer) => peer.disconnect(),
            Peer::NotConnected(_) => {}
            Peer::Unknown(_) => {}
        };
    }

    fn on_remove_from_peers_set(&mut self, set_id: SetId, peer_id: PeerId) {
        match self.data.peer(set_id.into(), &peer_id) {
            Peer::Connected(peer) => {
                self.message_queue
                    .push_back(Message::Drop { set_id, peer_id });
                peer.disconnect().forget_peer();
            }
            Peer::NotConnected(peer) => {
                peer.forget_peer();
            }
            Peer::Unknown(_) => {}
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

    fn get_peer_ids(
        &mut self,
        session_id: SessionId,
        pending_result: oneshot::Sender<Vec<PeerId>>,
    ) {
        let set_id = match self.data.sessions.get(&session_id) {
            Some(set_id) => *set_id,
            None => {
                drop(pending_result);
                return;
            }
        };

        let _ = pending_result.send(self.data.peer_ids(set_id));
    }

    fn register_session(
        &mut self,
        session_id: SessionId,
        set_id: SetId,
        target_size: SetSize,
        on_ready: oneshot::Sender<()>,
    ) {
        match self.data.sessions.entry(session_id) {
            Entry::Vacant(e) => {
                e.insert(set_id.into());
            }
            _ => {}
        };

        let peers = self.data.peer_ids(set_id.into());

        self.message_queue.push_back(Message::GatherSet {
            protocol_id,
            session_id,
            target_size,
            on_ready,
        });
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
                Action::AddToPeersSet(set_id, peer_id) => self.add_to_peers_set(set_id, peer_id),
                Action::RemoveFromPeersSet(set_id, peer_id) => {
                    self.on_remove_from_peers_set(set_id, peer_id)
                }
                Action::GetPeerIndex(session_id, peer_id, pending_result) => {
                    self.get_peer_index(session_id, peer_id, pending_result)
                }
                Action::GetPeerAtIndex(session_id, index, pending_result) => {
                    self.get_peer_id(session_id, index, pending_result)
                }
                Action::GetPeerIds(session_id, pending_result) => {
                    self.get_peer_ids(session_id, pending_result)
                }
                Action::RegisterSession(session_id, set_id, target_size, on_ready) => {
                    self.register_session(session_id, set_id, target_size, on_ready)
                }
            }
        }
    }
}
