// This file was a part of Substrate.
// broadcast.rc <> request_response.rc

// Copyright (C) 2019-2022 Parity Technologies (UK) Ltd.
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

use crate::broadcast;
use futures::channel::oneshot;
use libp2p::swarm::NetworkBehaviourEventProcess;
use libp2p::swarm::{NetworkBehaviour, NetworkBehaviourAction, PollParameters};
use libp2p::{Multiaddr, NetworkBehaviour};
use libp2p::PeerId;
use log::{debug, error, info};
use mpc_peerset::Peerset;
use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

#[derive(NetworkBehaviour)]
#[behaviour(out_event = "BehaviourOut", poll_method = "poll", event_process = true)]
pub(crate) struct Behaviour {
    pub message_broadcast: broadcast::GenericBroadcast,

    #[behaviour(ignore)]
    events: VecDeque<BehaviourOut>,
    #[behaviour(ignore)]
    peerset: Peerset,
    #[behaviour(ignore)]
    addresses: HashMap<PeerId, Multiaddr>,
}

pub(crate) enum BehaviourOut {
    InboundMessage {
        /// Peer which sent us a message.
        peer: PeerId,
        /// Protocol name of the request.
        protocol: Cow<'static, str>,
    },
}

impl Behaviour {
    pub fn new(
        broadcast_protocols: Vec<broadcast::ProtocolConfig>,
        peerset: Peerset,
        addresses: HashMap<PeerId, Multiaddr>
    ) -> Result<Behaviour, broadcast::RegisterError> {
        Ok(Behaviour {
            message_broadcast: broadcast::GenericBroadcast::new(
                broadcast_protocols.into_iter(),
                peerset.get_handle(),
            )?,
            events: VecDeque::new(),
            peerset,
            addresses
        })
    }

    /// Initiates direct sending of a message.
    pub fn send_message(
        &mut self,
        peer: &PeerId,
        protocol: &str,
        message: Vec<u8>,
        pending_response: oneshot::Sender<Result<Vec<u8>, broadcast::RequestFailure>>,
        connect: broadcast::IfDisconnected,
    ) {
        self.message_broadcast
            .send_message(peer, protocol, message, pending_response, connect)
    }

    /// Initiates broadcasting of a message.
    pub fn broadcast_message(
        &mut self,
        protocol: &str,
        message: Vec<u8>,
        pending_response: oneshot::Sender<Result<Vec<u8>, broadcast::RequestFailure>>,
        connect: broadcast::IfDisconnected,
    ) {
        self.message_broadcast.broadcast_message(
            self.peerset.state().connected_peers(),
            protocol,
            message,
            pending_response,
            connect,
        );
    }

    /// Consumes the events list when polled.
    fn poll(
        &mut self,
        cx: &mut Context,
        _: &mut impl PollParameters,
    ) -> Poll<NetworkBehaviourAction<BehaviourOut, <Self as NetworkBehaviour>::ProtocolsHandler>>
    {
        loop {
            match futures::Stream::poll_next(Pin::new(&mut self.peerset), cx) {
                Poll::Ready(Some(mpc_peerset::Message::Connect(peer_id))) => {
                    // todo: self.peerset_report_connect(peer_id);
                },
                Poll::Ready(Some(mpc_peerset::Message::Drop(peer_id))) => {
                    // todo: self.peerset_report_disconnect(peer_id, set_id);
                },
                Poll::Ready(None) => {
                    error!(target: "sub-libp2p", "Peerset receiver stream has returned None");
                    break
                },
                Poll::Pending => break,
            }
        }

        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(event));
        }

        Poll::Pending
    }

    pub fn mark_peer_connected(&mut self, peer_id: PeerId) {
        let peer = self.peerset.state_mut().peer(&peer_id);
        if let Some(not_connected) = peer.into_not_connected() {
            if not_connected.try_accept_peer().is_ok() {
                println!("Peer {} marked as connected", peer_id.to_base58())
            } else {
                error!("Failed dealing peer {}", peer_id.to_base58());
            }
        }

        let conns: Vec<&PeerId> = self.peerset.state().connected_peers().collect();
        println!("connected peers: {}", conns.len())
    }
}

impl NetworkBehaviourEventProcess<broadcast::Event> for Behaviour {
    fn inject_event(&mut self, event: broadcast::Event) {
        match event {
            broadcast::Event::InboundMessage {
                peer,
                protocol,
                result,
            } => {
                self.events
                    .push_back(BehaviourOut::InboundMessage { peer, protocol });
            }
            broadcast::Event::BroadcastFinished {
                peer,
                protocol,
                duration,
                result,
            } => {
                debug!(
                    "broadcast for protocol {:?} finished to {:?} peer: {:?} took: {:?}",
                    protocol.to_string(),
                    peer,
                    result,
                    duration
                );
            }
            broadcast::Event::ReputationChanges { peer, changes } => {
                for change in changes {
                    debug!("reputation changed for {:?} peer: {:?}", peer, change);
                }
            }
        }
    }
}
