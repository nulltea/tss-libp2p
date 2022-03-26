use std::borrow::Cow;
use std::collections::VecDeque;
use std::iter;
use std::task::{Context, Poll};
use std::time::Duration;
use futures::channel::oneshot;
use futures::Sink;
use libp2p::identity::Keypair;
use libp2p::{Multiaddr, PeerId};
use libp2p::core::transport::memory::Channel;
use libp2p::NetworkBehaviour;
use libp2p::swarm::{NetworkBehaviour, NetworkBehaviourAction, PollParameters};
use libp2p::request_response::{
    RequestId, RequestResponse,
    RequestResponseEvent::{self, InboundFailure, Message, OutboundFailure},
    RequestResponseMessage::{Request, Response},
    ResponseChannel, RequestResponseConfig, ProtocolSupport
};
use libp2p::swarm::NetworkBehaviourEventProcess;
use log::{debug, error, trace};
use serde::de::DeserializeOwned;
use serde::Serialize;
use mpc_peerset::{Peerset, PeersetHandle, SetId};
use crate::{broadcast};
use crate::mpc_protocol::{MPCProtocol, MPCProtocolCodec, WireResponse};

#[derive(NetworkBehaviour)]
#[behaviour(out_event = "BehaviourOut", poll_method = "poll", event_process = true)]
pub(crate) struct Behaviour {
    pub message_broadcast: broadcast::GenericBroadcast,

    #[behaviour(ignore)]
    events: VecDeque<BehaviourOut>,
    #[behaviour(ignore)]
    peerset: &'static mut Peerset,
}

pub(crate) enum BehaviourOut {
    InboundMessage {
        /// Peer which sent us a message.
        peer: PeerId,
        /// Protocol name of the request.
        protocol: Cow<'static, str>,
    },
}

impl Behaviour
    where M: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    pub fn new(
        mut broadcast_protocols: Vec<broadcast::ProtocolConfig>,
        peerset: &'static mut Peerset,
    ) -> Behaviour {
        Behaviour {
            message_broadcast: broadcast::GenericBroadcast::new(
                broadcast_protocols.into_iter(),
                peerset.get_handle(),
            )?,
            events: VecDeque::new(),
            peerset
        }
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
        for peer in self.peerset.connected_peers(MAIN_PEERSET) {
            self.message_broadcast
                .send_message(peer, protocol, message.clone(),
                              pending_response.clone(), connect)
        }

    }

    /// Consumes the events list when polled.
    fn poll(
        &mut self,
        _cx: &mut Context,
        _: &mut impl PollParameters,
    ) -> Poll<NetworkBehaviourAction<BehaviourOut, <Self as NetworkBehaviour>::ProtocolsHandler>>
    {
        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(event))
        }

        Poll::Pending
    }
}

impl NetworkBehaviourEventProcess<broadcast::Event> for Behaviour
{
    fn inject_event(&mut self, event: broadcast::Event) {
        match event {
            broadcast::Event::InboundMessage { peer, protocol, result } => {
                self.events.push_back(BehaviourOut::InboundMessage { peer, protocol });
            },
            broadcast::Event::BroadcastFinished { peer, protocol, duration, result } => {
                debug!("broadcast for protocol {:?} finished to {:?} peer: {:?}", protocol.to_string(), peer, changes);
            },
            broadcast::Event::ReputationChanges { peer, changes } =>
                for change in changes {
                    debug!("reputation changed for {:?} peer: {:?}", peer, changes);
                },
        }
    }
}
