use crate::state::{Node, PeersState};
use libp2p::PeerId;
use log::{debug, error};
use std::borrow::Cow;
use std::time::Instant;

/// Grants access to the state of a peer in the [`PeersState`] in the context of a specific set.
pub enum Peer<'a> {
    /// We are connected to this node.
    Connected(ConnectedPeer<'a>),
    /// We are not connected to this node.
    NotConnected(NotConnectedPeer<'a>),
    /// We have never heard of this node, or it is not part of the set.
    Unknown(UnknownPeer<'a>),
}

impl<'a> Peer<'a> {
    /// If we are the `Connected` variant, returns the inner [`ConnectedPeer`]. Returns `None`
    /// otherwise.
    pub fn into_connected(self) -> Option<ConnectedPeer<'a>> {
        match self {
            Self::Connected(peer) => Some(peer),
            Self::NotConnected(..) | Self::Unknown(..) => None,
        }
    }

    /// If we are the `NotConnected` variant, returns the inner [`NotConnectedPeer`]. Returns `None`
    /// otherwise.
    pub fn into_not_connected(self) -> Option<NotConnectedPeer<'a>> {
        match self {
            Self::NotConnected(peer) => Some(peer),
            Self::Connected(..) | Self::Unknown(..) => None,
        }
    }

    /// If we are the `Unknown` variant, returns the inner [`UnknownPeer`]. Returns `None`
    /// otherwise.
    pub fn into_unknown(self) -> Option<UnknownPeer<'a>> {
        match self {
            Self::Unknown(peer) => Some(peer),
            Self::Connected(..) | Self::NotConnected(..) => None,
        }
    }
}

/// A peer that is connected to us.
pub struct ConnectedPeer<'a> {
    pub(crate) set: usize,
    pub(crate) state: &'a mut PeersState,
    pub(crate) peer_id: Cow<'a, PeerId>,
}

impl<'a> ConnectedPeer<'a> {
    /// Get the `PeerId` associated to this `ConnectedPeer`.
    pub fn peer_id(&self) -> &PeerId {
        &self.peer_id
    }

    /// Destroys this `ConnectedPeer` and returns the `PeerId` inside of it.
    pub fn into_peer_id(self) -> PeerId {
        self.peer_id.into_owned()
    }

    /// Switches the peer to "not connected".
    pub fn disconnect(self) -> NotConnectedPeer<'a> {
        if let Some(node) = self.state.nodes.get_mut(&*self.peer_id) {
            match node.sets[self.set] {
                MembershipState::Connected => self.state.sets[self.set].num_peers -= 1,
                MembershipState::NotMember | MembershipState::NotConnected { .. } => {
                    debug_assert!(
                        false,
                        "State inconsistency: disconnecting a disconnected node"
                    )
                }
            }
            node.sets[self.set] = MembershipState::NotConnected {
                last_connected: Instant::now(),
            };
        } else {
            debug!("State inconsistency: disconnecting a disconnected node");
        }

        NotConnectedPeer {
            set: self.set,
            state: self.state,
            peer_id: self.peer_id,
        }
    }
}

/// A peer that is not connected to us.
#[derive(Debug)]
pub struct NotConnectedPeer<'a> {
    pub(crate) set: usize,
    pub(crate) state: &'a mut PeersState,
    pub(crate) peer_id: Cow<'a, PeerId>,
}

impl<'a> NotConnectedPeer<'a> {
    /// Destroys this `NotConnectedPeer` and returns the `PeerId` inside of it.
    pub fn into_peer_id(self) -> PeerId {
        self.peer_id.into_owned()
    }

    /// Tries to accept the peer as an incoming connection.
    pub fn try_accept_peer(self) -> Result<ConnectedPeer<'a>, Self> {
        if self.state.sets[self.set].num_peers >= self.state.sets[self.set].target_size {
            return Err(self);
        }

        if let Some(node) = self.state.nodes.get_mut(&*self.peer_id) {
            node.sets[self.set] = MembershipState::Connected;
            self.state.sets[self.set].num_peers += 1;
        } else {
            debug!("State inconsistency: try_accept_incoming on an unknown node");
        }

        Ok(ConnectedPeer {
            set: self.set,
            state: self.state,
            peer_id: self.peer_id,
        })
    }

    /// Removes the peer from the list of members of the set.
    pub fn forget_peer(self) -> UnknownPeer<'a> {
        if let Some(node) = self.state.nodes.get_mut(&*self.peer_id) {
            node.sets[self.set] = MembershipState::NotMember;

            // Remove the peer from `self.state.nodes` entirely if it isn't a member of any set.
            if peer
                .sets
                .iter()
                .all(|set| matches!(set, MembershipState::NotMember))
            {
                self.state.nodes.remove(&*self.peer_id);
            }
        } else {
            error!(
                target: "peerset",
                "State inconsistency with {} when forgetting peer",
                self.peer_id
            );
        };

        UnknownPeer {
            set: self.set,
            parent: self.state,
            peer_id: self.peer_id,
        }
    }
}

/// A peer that we have never heard of or that isn't part of the set.
pub struct UnknownPeer<'a> {
    pub(crate) set: usize,
    pub(crate) parent: &'a mut PeersState,
    pub(crate) peer_id: Cow<'a, PeerId>,
}

impl<'a> UnknownPeer<'a> {
    /// Inserts the peer identity in our list.
    pub fn discover(self) -> NotConnectedPeer<'a> {
        let num_sets = self.parent.sets.len();

        self.parent
            .nodes
            .entry(self.peer_id.clone().into_owned())
            .or_insert_with(|| Node::new(num_sets))
            .sets[self.set] = MembershipState::NotConnected {
            last_connected: Instant::now(),
        };

        NotConnectedPeer {
            set: self.set,
            state: self.parent,
            peer_id: self.peer_id,
        }
    }
}

/// Whether we are connected to a node in the context of a specific set.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum MembershipState {
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
    pub fn is_connected(self) -> bool {
        match self {
            Self::Connected => true,
            Self::NotMember | Self::NotConnected { .. } => false,
        }
    }

    /// Returns `true` for [`MembershipState::NotConnected`].
    pub fn is_not_connected(self) -> bool {
        matches!(self, Self::NotConnected { .. })
    }
}
