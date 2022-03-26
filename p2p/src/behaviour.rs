use std::iter;
use std::time::Duration;
use anyhow::Context;
use futures::Sink;
use libp2p::identity::Keypair;
use libp2p::{Multiaddr, PeerId};
use libp2p::NetworkBehaviour;
use libp2p::swarm::NetworkBehaviour;
use libp2p::request_response::{
    RequestId, RequestResponse,
    RequestResponseEvent::{self, InboundFailure, Message, OutboundFailure},
    RequestResponseMessage::{Request, Response},
    ResponseChannel, RequestResponseConfig, ProtocolSupport
};
use libp2p::swarm::NetworkBehaviourEventProcess;
use round_based::{async_runtime::AsyncProtocol, Msg};
use serde::de::DeserializeOwned;
use serde::Serialize;
use crate::{MPCCodec, MPCProtocol, WireRequest, WireResponse, MPCResult, MPCRecord, P2PConfig};

#[derive(NetworkBehaviour)]
#[behaviour(event_process = true)]
pub struct MultiPartyBehaviour {
    msg_proto: RequestResponse<MPCCodec>,

    #[behaviour(ignore)]
    peer_ids: Vec<(PeerId, Multiaddr)>,
}

impl MultiPartyBehaviour {
    pub fn new(
        keypair: &Keypair,
        peer_ids: Vec<(PeerId, Multiaddr)>,
        network_name: &str,
    ) -> MultiPartyBehaviour {
        let msg_proto = {
            let cfg = RequestResponseConfig::default();
            let protocols = iter::once((MPCProtocol(), ProtocolSupport::Full));
            RequestResponse::new(MPCCodec(), protocols, cfg)
        };

        MultiPartyBehaviour {
            peer_ids,
            msg_proto
        }
    }
}

impl NetworkBehaviourEventProcess<RequestResponseEvent<WireRequest, WireResponse>> for MultiPartyBehaviour
{
    // Called when the mpc_protocol produces an event.
    fn inject_event(&mut self, event: RequestResponseEvent<WireRequest, WireResponse>) {
        match event {
            Message { peer: _, message } => match message {
                Request {
                    request_id: _,
                    request,
                    channel,
                } => self.handle_request_msg(request, channel),
                Response {
                    request_id,
                    response,
                } => self.handle_response_msg(request_id, response),
            },
            OutboundFailure {
                peer,
                request_id,
                error,
            } => println!(
                "Outbound Failure for request {:?} to peer: {:?}: {:?}.",
                request_id, peer, error
            ),
            InboundFailure {
                peer,
                request_id,
                error,
            } => println!(
                "Inbound Failure for request {:?} to peer: {:?}: {:?}.",
                request_id, peer, error
            ),
            _ => {}
        }
    }
}

impl MultiPartyBehaviour {
    fn handle_request_msg(
        &mut self,
        request: WireRequest,
        channel: ResponseChannel<WireResponse>,
    ) {
        match request {
            WireRequest::Message(r) => {
                let duration = if r.timeout_sec > 0 {
                    r.timeout_sec
                } else {
                    9000u64
                };

                println!("Successfully stored record.");
                self.msg_proto
                    .send_response(channel, WireResponse::Response(MPCResult::Success));

            },
            Ping => {
                println!("Received Ping, we will send a Pong back.");
                self.msg_proto.send_response(channel, WireResponse::Pong);
            }
        }
    }

    fn handle_response_msg(&mut self, request_id: RequestId, response: WireResponse) {
        match response {
            WireResponse::Pong => {
                println!("Received Pong for request {:?}.", request_id);
            }
            WireResponse::Response(result) => {
                println!(
                    "Received Result for publish request {:?}: {:?}.",
                    request_id, result
                );
            }
        }
    }
}
