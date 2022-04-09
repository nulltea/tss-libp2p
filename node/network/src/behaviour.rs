use crate::broadcast;
use futures::channel::oneshot;
use libp2p::identify::{Identify, IdentifyConfig, IdentifyEvent};
use libp2p::identity::Keypair;
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
    pub(crate) message_broadcast: broadcast::GenericBroadcast,
    identify: Identify,

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
        broadcast_protocols: Vec<broadcast::ProtocolConfig>,
        peerset: Peerset,
    ) -> Result<Behaviour, broadcast::RegisterError> {
        Ok(Behaviour {
            message_broadcast: broadcast::GenericBroadcast::new(
                broadcast_protocols.into_iter(),
                peerset.get_handle(),
            )?,
            identify: Identify::new(IdentifyConfig::new(
                MPC_PROTOCOL_ID.into(),
                local_key.public(),
            )),
            events: VecDeque::new(),
            peerset,
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
                Poll::Ready(Some(mpc_peerset::Message::Connect(addr))) => {
                    return Poll::Ready(NetworkBehaviourAction::DialAddress {
                        address: addr,
                        handler: self.new_handler(),
                    })
                }
                Poll::Ready(Some(mpc_peerset::Message::Drop(peer_id))) => {
                    return Poll::Ready(NetworkBehaviourAction::CloseConnection {
                        peer_id,
                        connection: CloseConnection::All,
                    })
                }
                Poll::Ready(None) => {
                    error!(target: "sub-libp2p", "Peerset receiver stream has returned None");
                    break;
                }
                Poll::Pending => break,
            }
        }

        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(event));
        }

        Poll::Pending
    }

    pub fn mark_peer_as_connected(&mut self, peer_id: PeerId) {
        let peer = self.peerset.state_mut().peer(&peer_id);
        if let Some(not_connected) = peer.into_not_connected() {
            if not_connected.try_accept_peer().is_ok() {
                debug!("Peer {} marked as connected", peer_id.to_base58());
            } else {
                warn!(
                    "Peer {} was already marked as connected",
                    peer_id.to_base58()
                );
            }
        }
    }

    pub fn mark_peer_as_disconnected(&mut self, peer_id: PeerId) {
        let peer = self.peerset.state_mut().peer(&peer_id);
        if let Some(connected) = peer.into_connected() {
            connected.disconnect();
            debug!("Peer {} marked as disconnected", peer_id.to_base58())
        }
    }

    pub fn peer_membership(&self, peer_id: &PeerId) -> mpc_peerset::MembershipState {
        self.peerset.state().peer_membership(peer_id)
    }

    pub fn peer_at_index(&self, index: usize) -> Option<PeerId> {
        self.peerset.state().at_index(index)
    }
}

impl NetworkBehaviourEventProcess<broadcast::Event> for Behaviour {
    fn inject_event(&mut self, event: broadcast::Event) {
        match event {
            broadcast::Event::InboundMessage {
                peer,
                protocol,
                result: _,
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
