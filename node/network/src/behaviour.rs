use crate::broadcast::MessageContext;
use crate::discovery::{DiscoveryBehaviour, DiscoveryOut};
use crate::{broadcast, MessageType, Params, RoomArgs, RoomId};
use async_std::task;
use futures::channel::mpsc;
use libp2p::core::connection::ConnectionId;
use libp2p::identify::{Identify, IdentifyConfig, IdentifyEvent};
use libp2p::identity::Keypair;
use libp2p::kad::store::MemoryStore;
use libp2p::kad::{
    Kademlia, KademliaConfig, KademliaEvent, PeerRecord, QueryId, QueryResult, Record,
};
use libp2p::mdns::{Mdns, MdnsEvent};
use libp2p::ping::{Ping, PingEvent, PingFailure, PingSuccess};
use libp2p::swarm::toggle::Toggle;
use libp2p::swarm::{CloseConnection, DialPeerCondition, NetworkBehaviourEventProcess};
use libp2p::swarm::{NetworkBehaviour, NetworkBehaviourAction, PollParameters};
use libp2p::NetworkBehaviour;
use libp2p::PeerId;
use log::{debug, error, trace, warn};
use smallvec::SmallVec;
use std::borrow::Cow;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::pin::Pin;
use std::task::{Context, Poll};

const MPC_PROTOCOL_ID: &str = "/mpc/0.1.0";

#[derive(NetworkBehaviour)]
#[behaviour(out_event = "BehaviourOut", poll_method = "poll", event_process = true)]
pub(crate) struct Behaviour {
    ping: Ping,
    identify: Identify,
    discovery: DiscoveryBehaviour,
    /// Handles multiple communication of multiple generic protocols.
    broadcast: broadcast::Broadcast,

    #[behaviour(ignore)]
    events: VecDeque<BehaviourOut>,
}

pub(crate) enum BehaviourOut {
    InboundMessage {
        /// Peer which sent us a message.
        peer: PeerId,
        /// Protocol name of the request.
        room_id: Cow<'static, str>,
    },
}

impl Behaviour {
    pub fn new(
        local_key: &Keypair,
        rooms: impl Iterator<Item = RoomArgs>,
        broadcast_protocols: Vec<broadcast::ProtocolConfig>,
    ) -> Result<Behaviour, broadcast::RegisterError> {
        Ok(Behaviour {
            broadcast: broadcast::Broadcast::new(broadcast_protocols.into_iter())?,
            discovery: DiscoveryBehaviour::new(local_key.public(), rooms),
            identify: Identify::new(IdentifyConfig::new(
                MPC_PROTOCOL_ID.into(),
                local_key.public(),
            )),
            ping: Ping::default(),
            events: VecDeque::new(),
        })
    }

    /// Initiates direct sending of a message.
    pub fn send_message(
        &mut self,
        peer: &PeerId,
        message: Vec<u8>,
        room_id: RoomId,
        ctx: MessageContext,
        pending_response: mpsc::Sender<Result<(PeerId, Vec<u8>), broadcast::RequestFailure>>,
        connect: broadcast::IfDisconnected,
    ) {
        self.broadcast.send_message(
            peer,
            room_id.as_protocol_id(),
            ctx,
            message,
            pending_response,
            connect,
        )
    }

    /// Initiates broadcasting of a message.
    pub fn broadcast_message(
        &mut self,
        peer_ids: impl Iterator<Item = PeerId>,
        message: Vec<u8>,
        room_id: RoomId,
        ctx: MessageContext,
        pending_response: Option<
            mpsc::Sender<Result<(PeerId, Vec<u8>), broadcast::RequestFailure>>,
        >,
        connect: broadcast::IfDisconnected,
    ) {
        self.broadcast.broadcast_message(
            peer_ids,
            room_id.as_protocol_id(),
            ctx,
            message,
            pending_response,
            connect,
        );
    }

    /// Bootstrap Kademlia network.
    pub fn bootstrap(&mut self) -> Result<QueryId, String> {
        self.discovery.bootstrap()
    }

    /// Bootstrap Kademlia network.
    pub fn peers(&mut self, _room_id: RoomId) -> impl Iterator<Item = PeerId> {
        self.discovery.peers()
    }

    /// Consumes the events list when polled.
    fn poll(
        &mut self,
        _: &mut Context,
        _: &mut impl PollParameters,
    ) -> Poll<NetworkBehaviourAction<BehaviourOut, <Self as NetworkBehaviour>::ProtocolsHandler>>
    {
        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(event));
        }

        Poll::Pending
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
                self.events.push_back(BehaviourOut::InboundMessage {
                    peer,
                    room_id: protocol,
                });
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
            DiscoveryOut::Connected(..) => {}
            DiscoveryOut::Disconnected(..) => {}
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
