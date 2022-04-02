use std::borrow::Cow;
use std::time::Instant;
use libp2p::PeerId;
use log::error;
use std::collections::hash_map::OccupiedEntry;
use crate::state::{Node, PeersState, MembershipState};

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
            match node.set {
                MembershipState::Connected => self.state.set.num_peers -= 1,
                MembershipState::NotMember | MembershipState::NotConnected { .. } => {
                    debug_assert!(
                        false,
                        "State inconsistency: disconnecting a disconnected node"
                    )
                }
            }
            node.set = MembershipState::NotConnected {
                last_connected: Instant::now(),
            };
        } else {
            debug_assert!(
                false,
                "State inconsistency: disconnecting a disconnected node"
            );
        }

        NotConnectedPeer {
            state: self.state,
            peer_id: self.peer_id,
        }
    }

    /// Performs an arithmetic addition on the reputation score of that peer.
    ///
    /// In case of overflow, the value will be capped.
    ///
    /// > **Note**: Reputation values aren't specific to a set but are global per peer.
    pub fn add_reputation(&mut self, modifier: i32) {
        if let Some(node) = self.state.nodes.get_mut(&*self.peer_id) {
            node.reputation = node.reputation.saturating_add(modifier);
        } else {
            debug_assert!(
                false,
                "State inconsistency: add_reputation on an unknown node"
            );
        }
    }

    /// Returns the reputation value of the node.
    ///
    /// > **Note**: Reputation values aren't specific to a set but are global per peer.
    pub fn reputation(&self) -> i32 {
        self.state
            .nodes
            .get(&*self.peer_id)
            .map_or(0, |p| p.reputation)
    }
}

/// A peer that is not connected to us.
#[derive(Debug)]
pub struct NotConnectedPeer<'a> {
    pub(crate) state: &'a mut PeersState,
    pub(crate) peer_id: Cow<'a, PeerId>,
}

impl<'a> NotConnectedPeer<'a> {
    /// Destroys this `NotConnectedPeer` and returns the `PeerId` inside of it.
    pub fn into_peer_id(self) -> PeerId {
        self.peer_id.into_owned()
    }

    /// Bumps the value that `last_connected_or_discovered` would return to now, even if we
    /// didn't connect or disconnect.
    pub fn bump_last_connected_or_discovered(&mut self) {
        let state = match self.state.nodes.get_mut(&*self.peer_id) {
            Some(s) => s,
            None => return,
        };

        if let MembershipState::NotConnected { last_connected } = &mut state.set {
            *last_connected = Instant::now();
        }
    }

    /// Returns when we were last connected to this peer, or when we discovered it if we were
    /// never connected.
    ///
    /// Guaranteed to be earlier than calling `Instant::now()` after the function returns.
    pub fn last_connected_or_discovered(&self) -> Instant {
        let state = match self.state.nodes.get(&*self.peer_id) {
            Some(s) => s,
            None => {
                error!(
                    target: "peerset",
                    "State inconsistency with {}; not connected after borrow",
                    self.peer_id
                );
                return Instant::now();
            }
        };

        match state.set {
            MembershipState::NotConnected { last_connected } => last_connected,
            _ => {
                error!(target: "peerset", "State inconsistency with {}", self.peer_id);
                Instant::now()
            }
        }
    }

    /// Tries to accept the peer as an incoming connection.
    ///
    /// If there are enough slots available, switches the node to "connected" and returns `Ok`. If
    /// the slots are full, the node stays "not connected" and we return `Err`.
    ///
    /// Non-slot-occupying nodes don't count towards the number of slots.
    pub fn try_accept_peer(self) -> Result<ConnectedPeer<'a>, Self> {
        // Note that it is possible for num_peers to be strictly superior to the max, in case we were
        // connected to reserved node then marked them as not reserved.
        if self.state.set.num_peers >= self.state.set.max_peers {
            return Err(self);
        }

        if let Some(peer) = self.state.nodes.get_mut(&*self.peer_id) {
            peer.set = MembershipState::Connected;
            self.state.set.num_peers += 1;
        } else {
            debug_assert!(
                false,
                "State inconsistency: try_accept_incoming on an unknown node"
            );
        }

        Ok(ConnectedPeer {
            state: self.state,
            peer_id: self.peer_id,
        })
    }

    /// Returns the reputation value of the node.
    ///
    /// > **Note**: Reputation values aren't specific to a set but are global per peer.
    pub fn reputation(&self) -> i32 {
        self.state
            .nodes
            .get(&*self.peer_id)
            .map_or(0, |p| p.reputation)
    }

    /// Sets the reputation of the peer.
    ///
    /// > **Note**: Reputation values aren't specific to a set but are global per peer.
    #[cfg(test)] // Feel free to remove this if this function is needed outside of tests
    pub fn set_reputation(&mut self, value: i32) {
        if let Some(node) = self.state.nodes.get_mut(&*self.peer_id) {
            node.reputation = value;
        } else {
            debug_assert!(
                false,
                "State inconsistency: set_reputation on an unknown node"
            );
        }
    }

    /// Removes the peer from the list of members of the set.
    pub fn forget_peer(self) -> UnknownPeer<'a> {
        if let Some(peer) = self.state.nodes.get_mut(&*self.peer_id) {
            debug_assert!(!matches!(peer.set, MembershipState::NotMember));
            peer.set = MembershipState::NotMember;

            // Remove the peer from `self.state.nodes` entirely if it isn't a member of any set.
            if peer.reputation == 0 && matches!(peer.set, MembershipState::NotMember) {
                self.state.nodes.remove(&*self.peer_id);
            }
        } else {
            debug_assert!(false, "State inconsistency: forget_peer on an unknown node");
            error!(
                target: "peerset",
                "State inconsistency with {} when forgetting peer",
                self.peer_id
            );
        };

        UnknownPeer {
            parent: self.state,
            peer_id: self.peer_id,
        }
    }
}

/// A peer that we have never heard of or that isn't part of the set.
pub struct UnknownPeer<'a> {
    pub(crate) parent: &'a mut PeersState,
    pub(crate) peer_id: Cow<'a, PeerId>,
}

impl<'a> UnknownPeer<'a> {
    /// Inserts the peer identity in our list.
    ///
    /// The node starts with a reputation of 0. You can adjust these default
    /// values using the `NotConnectedPeer` that this method returns.
    pub fn discover(self) -> NotConnectedPeer<'a> {
        self.parent
            .nodes
            .entry(self.peer_id.clone().into_owned())
            .or_insert_with(|| Node::new())
            .set = MembershipState::NotConnected {
            last_connected: Instant::now(),
        };

        NotConnectedPeer {
            state: self.parent,
            peer_id: self.peer_id,
        }
    }
}

/// Access to the reputation of a peer.
pub struct Reputation<'a> {
    /// Node entry in [`PeersState::nodes`]. Always `Some` except right before dropping.
    pub(crate) node: Option<OccupiedEntry<'a, PeerId, Node>>,
}

impl<'a> Reputation<'a> {
    /// Returns the reputation value of the node.
    pub fn reputation(&self) -> i32 {
        self.node.as_ref().unwrap().get().reputation
    }

    /// Sets the reputation of the peer.
    pub fn set_reputation(&mut self, value: i32) {
        self.node.as_mut().unwrap().get_mut().reputation = value;
    }

    /// Performs an arithmetic addition on the reputation score of that peer.
    ///
    /// In case of overflow, the value will be capped.
    pub fn add_reputation(&mut self, modifier: i32) {
        let reputation = &mut self.node.as_mut().unwrap().get_mut().reputation;
        *reputation = reputation.saturating_add(modifier);
    }
}

impl<'a> Drop for Reputation<'a> {
    fn drop(&mut self) {
        if let Some(node) = self.node.take() {
            if node.get().reputation == 0 && matches!(node.get().set, MembershipState::NotMember) {
                node.remove();
            }
        }
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
