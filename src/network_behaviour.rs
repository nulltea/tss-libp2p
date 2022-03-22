use std::iter;
use std::time::Duration;
use anyhow::Context;
use futures::Sink;
use libp2p::PeerId;
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
use crate::mpc_protocol::{MPCCodec, MPCProtocol, MPCMessage, MPCResponse, MPCResult, MPCRecord};

#[derive(NetworkBehaviour)]
#[behaviour(event_process = true)]
pub struct RoundBasedBehaviour {
    msg_proto: RequestResponse<MPCCodec>,

    #[behaviour(ignore)]
    peer_ids: Vec<PeerId>,
}

impl RoundBasedBehaviour {
    pub fn new(peer_ids: Vec<PeerId>) -> RoundBasedBehaviour {
        let msg_proto = {
            // set request_timeout and connection_keep_alive if necessary
            let cfg = RequestResponseConfig::default();
            let protocols = iter::once((MPCProtocol(), ProtocolSupport::Full));
            RequestResponse::new(MPCCodec(), protocols, cfg)
        };

        RoundBasedBehaviour{
            peer_ids,
            msg_proto
        }
    }
}

impl NetworkBehaviourEventProcess<RequestResponseEvent<MPCMessage, MPCResponse>> for RoundBasedBehaviour
{
    // Called when the mpc_protocol produces an event.
    fn inject_event(&mut self, event: RequestResponseEvent<MPCMessage, MPCResponse>) {
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

impl RoundBasedBehaviour {
    fn handle_request_msg(
        &mut self,
        request: MPCMessage,
        channel: ResponseChannel<MPCResponse>,
    ) {
        match request {
            MPCMessage::Message(r) => {
                let duration = if r.timeout_sec > 0 {
                    r.timeout_sec
                } else {
                    9000u64
                };

                println!("Successfully stored record.");
                self.msg_proto
                    .send_response(channel, MPCResponse::Response(MPCResult::Success));

            },
            Ping => {
                println!("Received Ping, we will send a Pong back.");
                self.msg_proto.send_response(channel, MPCResponse::Pong);
            }
        }
    }

    fn handle_response_msg(&mut self, request_id: RequestId, response: MPCResponse) {
        match response {
            MPCResponse::Pong => {
                println!("Received Pong for request {:?}.", request_id);
            }
            MPCResponse::Response(result) => {
                println!(
                    "Received Result for publish request {:?}: {:?}.",
                    request_id, result
                );
            }
        }
    }

    pub fn broadcast_fn<'a, M>(&'a mut self) -> impl Sink<Msg<M>, Error = anyhow::Error> + 'a where M: 'a + Serialize + DeserializeOwned, {
        futures::sink::unfold(self, |behaviour, message: Msg<M>| async move {
            let serialized = serde_json::to_string(&message).context("serialize message")?;
            for peer in &behaviour.peer_ids { //
                behaviour.msg_proto.send_request(peer,MPCMessage::Message(MPCRecord{
                    key: "MSG".to_string(),
                    timeout_sec: 100,
                    value: serialized.clone(),
                }));
            }
            Ok::<_, anyhow::Error>(behaviour)
        })
    }
}
