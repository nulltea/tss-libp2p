use crate::broadcast;
use crate::broadcast::ProtoContext;
use crate::discovery::{DiscoveryBehaviour, DiscoveryOut};
use futures::channel::mpsc;

use crate::coordination::CoordinationBehaviour;
use libp2p::identify::{Identify, IdentifyConfig, IdentifyEvent};
use libp2p::identity::Keypair;
use libp2p::kad::QueryId;
use libp2p::ping::{Ping, PingEvent, PingFailure, PingSuccess};
use libp2p::swarm::{CloseConnection, NetworkBehaviourEventProcess};
use libp2p::swarm::{NetworkBehaviour, NetworkBehaviourAction, PollParameters};
use libp2p::NetworkBehaviour;
use libp2p::PeerId;
use log::{debug, error, trace, warn};
use mpc_peerset::Peerset;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

const MPC_PROTOCOL_ID: &str = "/mpc/0.1.0";

#[derive(NetworkBehaviour)]
#[behaviour(out_event = "BehaviourOut", poll_method = "poll", event_process = true)]
pub(crate) struct Behaviour {
    ping: Ping,
    identify: Identify,
    discovery: DiscoveryBehaviour,
    coordination: CoordinationBehaviour,
    broadcast: broadcast::GenericBroadcast,

    #[behaviour(ignore)]
    events: VecDeque<BehaviourOut>,
    #[behaviour(ignore)]
    peerset: Peerset,
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
        local_key: &Keypair,
        params: &crate::Params,
        peerset: Peerset,
    ) -> Result<Behaviour, broadcast::RegisterError> {
        let (coordination, peerset_handle) =
            CoordinationBehaviour::new(params.network_config.clone());

        Ok(Behaviour {
            broadcast: broadcast::GenericBroadcast::new(
                params.broadcast_protocols.into_iter(),
                peerset.get_handle(),
            )?,
            discovery: DiscoveryBehaviour::new(local_key.public(), params.network_config.clone()),
            coordination,
            identify: Identify::new(IdentifyConfig::new(
                MPC_PROTOCOL_ID.into(),
                local_key.public(),
            )),
            ping: Ping::default(),
            events: VecDeque::new(),
            peerset,
        })
    }

    /// Initiates direct sending of a message.
    pub fn send_message(
        &mut self,
        peer: &PeerId,
        message: Vec<u8>,
        protocol_id: &str,
        ctx: ProtoContext,
        pending_response: mpsc::Sender<Result<Vec<u8>, broadcast::RequestFailure>>,
        connect: broadcast::IfDisconnected,
    ) {
        self.broadcast
            .send_message(peer, protocol_id, ctx, message, pending_response, connect)
    }

    /// Initiates broadcasting of a message.
    pub fn broadcast_message(
        &mut self,
        message: Vec<u8>,
        protocol_id: &str,
        ctx: ProtoContext,
        pending_response: mpsc::Sender<Result<Vec<u8>, broadcast::RequestFailure>>,
        connect: broadcast::IfDisconnected,
    ) {
        self.broadcast.broadcast_message(
            self.peerset.state().connected_peers(),
            protocol_id,
            ctx,
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
        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(event));
        }

        Poll::Pending
    }

    pub fn peer_at_index(&self, index: usize) -> Option<PeerId> {
        self.peerset.state().at_index(index)
    }

    /// Bootstrap Kademlia network
    pub fn bootstrap(&mut self) -> Result<QueryId, String> {
        self.discovery.bootstrap()
    }
}

impl NetworkBehaviourEventProcess<broadcast::BroadcastOut> for Behaviour {
    fn inject_event(&mut self, event: broadcast::BroadcastOut) {
        match event {
            broadcast::BroadcastOut::InboundMessage {
                peer,
                protocol,
                result: _,
            } => {
                self.events
                    .push_back(BehaviourOut::InboundMessage { peer, protocol });
            }
            broadcast::BroadcastOut::BroadcastFinished {
                peer,
                protocol,
                duration,
                result,
            } => {
                debug!(
                    "broadcast for protocol {:?} finished with {:?} peer: {:?} took: {:?}",
                    protocol.to_string(),
                    result,
                    peer,
                    duration
                );
            }
        }
    }
}

impl NetworkBehaviourEventProcess<DiscoveryOut> for Behaviour {
    fn inject_event(&mut self, event: DiscoveryOut) {
        match event {
            DiscoveryOut::Connected(peer_id) => self.peerset.add_to_peers_set(peer_id),
            DiscoveryOut::Disconnected(peer) => {
                self.peerset.get_handle().remove_from_peers_set(peer_id);
            }
        }
    }
}

impl NetworkBehaviourEventProcess<IdentifyEvent> for Behaviour {
    fn inject_event(&mut self, event: IdentifyEvent) {
        match event {
            IdentifyEvent::Received { peer_id, info } => {
                trace!("identified peer {:?}", peer_id);
                trace!("protocol_version {:?}", info.protocol_version);
                trace!("agent_version {:?}", info.agent_version);
                trace!("listen_addresses {:?}", info.listen_addrs);
                trace!("observed_address {:?}", info.observed_addr);
                trace!("protocols {:?}", info.protocols);
            }
            IdentifyEvent::Sent { .. } => (),
            IdentifyEvent::Pushed { .. } => (),
            IdentifyEvent::Error { .. } => (),
        }
    }
}

impl NetworkBehaviourEventProcess<PingEvent> for Behaviour {
    fn inject_event(&mut self, event: PingEvent) {
        match event.result {
            Ok(PingSuccess::Ping { rtt }) => {
                trace!(
                    "PingSuccess::Ping rtt to {} is {} ms",
                    event.peer.to_base58(),
                    rtt.as_millis()
                );
            }
            Ok(PingSuccess::Pong) => {
                trace!("PingSuccess::Pong from {}", event.peer.to_base58());
            }
            Err(PingFailure::Timeout) => {
                debug!("PingFailure::Timeout {}", event.peer.to_base58());
            }
            Err(PingFailure::Other { error }) => {
                debug!("PingFailure::Other {}: {}", event.peer.to_base58(), error);
            }
            Err(PingFailure::Unsupported) => {
                debug!("PingFailure::Unsupported {}", event.peer.to_base58());
            }
        }
    }
}
