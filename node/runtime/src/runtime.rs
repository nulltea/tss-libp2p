use crate::coordination::LocalRpcMsg;
use crate::coordination::Phase2Msg;
use crate::echo::EchoGadget;
use crate::execution::ProtocolExecution;
use crate::negotiation::NegotiationMsg;
use crate::peerset::Peerset;
use crate::{
    coordination, ComputeAgentAsync, PeersetCacher, PersistentCacher, ProtocolAgentFactory,
};
use anyhow::anyhow;
use blake2::Digest;
use futures::channel::{mpsc, oneshot};
use futures::StreamExt;
use futures_util::stream::FuturesUnordered;
use futures_util::{select, FutureExt, SinkExt};
use log::error;
use mpc_p2p::broadcast::OutgoingResponse;
use mpc_p2p::{broadcast, NetworkService, RoomId};
use std::collections::hash_map::Entry;
use std::collections::HashMap;

pub enum RuntimeMessage {
    RequestComputation {
        room_id: RoomId,
        n: u16,
        protocol_id: u64,
        args: Vec<u8>,
        on_done: oneshot::Sender<anyhow::Result<Vec<u8>>>,
    },
}

#[derive(Clone)]
pub struct RuntimeService {
    to_runtime: mpsc::Sender<RuntimeMessage>,
}

impl RuntimeService {
    pub async fn request_computation(
        &mut self,
        room_id: RoomId,
        n: u16,
        protocol_id: u64,
        args: Vec<u8>,
        on_done: oneshot::Sender<anyhow::Result<Vec<u8>>>,
    ) {
        self.to_runtime
            .send(RuntimeMessage::RequestComputation {
                room_id,
                n,
                protocol_id,
                args,
                on_done,
            })
            .await
            .expect("request computation expected");
    }
}

pub struct RuntimeDaemon<TFactory> {
    network_service: NetworkService,
    rooms: HashMap<RoomId, mpsc::Receiver<broadcast::IncomingMessage>>,
    agents_factory: TFactory,
    from_service: mpsc::Receiver<RuntimeMessage>,
    peerset_cacher: PersistentCacher,
}

impl<TFactory: ProtocolAgentFactory + Send + Unpin> RuntimeDaemon<TFactory> {
    pub fn new(
        network_service: NetworkService,
        rooms: impl Iterator<Item = (RoomId, mpsc::Receiver<broadcast::IncomingMessage>)>,
        agents_factory: TFactory,
        peerset_cacher: PersistentCacher,
    ) -> (Self, RuntimeService) {
        let (tx, rx) = mpsc::channel(2);

        let worker = Self {
            network_service,
            rooms: rooms.collect(),
            from_service: rx,
            agents_factory,
            peerset_cacher,
        };

        let service = RuntimeService { to_runtime: tx };

        (worker, service)
    }

    pub async fn run(self) {
        let mut protocol_executions = FuturesUnordered::new();
        let mut network_proxies = FuturesUnordered::new();
        let mut rooms_coordination = FuturesUnordered::new();
        let mut rooms_rpc = HashMap::new();

        let Self {
            network_service,
            rooms,
            agents_factory,
            from_service,
            mut peerset_cacher,
        } = self;

        for (room_id, rx) in rooms.into_iter() {
            let (ch, tx) =
                coordination::Phase1Channel::new(room_id.clone(), rx, network_service.clone());
            rooms_coordination.push(ch);
            rooms_rpc.insert(room_id, tx);
        }

        let mut service_messages = from_service.fuse();

        // loop {
        //     match rooms_coordination.select_next_some().await
        // }

        loop {
            select! {
                srv_msg = service_messages.select_next_some() => {
                    match srv_msg {
                        RuntimeMessage::RequestComputation{
                            room_id,
                            n,
                            protocol_id,
                            args,
                            on_done,
                        } => {
                            match rooms_rpc.entry(room_id) {
                                Entry::Occupied(e) => {
                                    let mut agent = match agents_factory.make(protocol_id) {
                                        Ok(a) => a,
                                        Err(_) => {
                                            on_done.send(Err(anyhow!("unknown protocol")));
                                            continue;
                                        }
                                    };
                                    let on_rpc = e.remove();

                                    if on_rpc.is_canceled() {
                                        on_done.send(Err(anyhow!("protocol is busy")));
                                    } else {
                                        on_rpc.send(LocalRpcMsg{n, args, agent, on_done});
                                    }
                                }
                                Entry::Vacant(_) => {
                                    error!("{:?}", on_done.send(Err(anyhow!("protocol is busy"))));
                                }
                            }
                        },
                    }
                },
                coord_msg = rooms_coordination.select_next_some() => match coord_msg {
                    coordination::Phase1Msg::FromRemote {
                        peer_id: _,
                        protocol_id,
                        session_id: _,
                        payload: _,
                        response_tx,
                        channel,
                    } => {
                        let agent = match agents_factory.make(protocol_id) {
                            Ok(a) => a,
                            Err(_) => {
                                let (id, ch, tx) = channel.abort();
                                rooms_coordination.push(ch);
                                rooms_rpc.insert(id, tx);
                                continue;
                            }
                        };

                        response_tx.send(OutgoingResponse {
                            result: Ok(vec![]), // todo: real negotiation logic
                            sent_feedback: None,
                        });

                        match channel.await {
                            Phase2Msg::Start {
                                room_id,
                                room_receiver,
                                receiver_proxy,
                                parties,
                                peerset_rx,
                                init_body,
                            } => {
                                network_proxies.push(receiver_proxy);
                                let (echo, echo_tx) = EchoGadget::new(parties.size());
                                protocol_executions.push(echo.wrap_execution(ProtocolExecution::new(
                                    room_id,
                                    init_body,
                                    agent,
                                    network_service.clone(),
                                    parties,
                                    peerset_rx,
                                    peerset_cacher.clone(),
                                    room_receiver,
                                    echo_tx,
                                    None,
                                )));
                            }
                            Phase2Msg::Abort(room_id, ch, tx) => {
                                rooms_rpc.entry(room_id).and_modify(|e| *e = tx);
                                rooms_coordination.push(ch);
                            }
                        }
                    }
                    coordination::Phase1Msg::FromLocal {
                        id,
                        n,
                        mut negotiation,
                    } => {
                        match negotiation.await {
                            NegotiationMsg::Start {
                                agent,
                                on_done,
                                room_receiver,
                                receiver_proxy,
                                parties,
                                peerset_rx,
                                args,
                            } => {
                                network_proxies.push(receiver_proxy);
                                let (echo, echo_tx) = EchoGadget::new(n as usize);
                                protocol_executions.push(echo.wrap_execution(ProtocolExecution::new(
                                    id,
                                    args,
                                    agent,
                                    network_service.clone(),
                                    parties,
                                    peerset_rx,
                                    peerset_cacher.clone(),
                                    room_receiver,
                                    echo_tx,
                                    Some(on_done),
                                )));
                            }
                            NegotiationMsg::Abort(room_id, phase1, rpc_tx) => {
                                rooms_coordination.push(phase1);
                                rooms_rpc.insert(room_id, rpc_tx);
                                continue;
                            }
                        };
                    }
                },
                exec_res = protocol_executions.select_next_some() => match exec_res {
                    Ok(_) => {}
                    Err(e) => {error!("error during computation: {e}")}
                },
                (room_id, phase1, rpc_tx) = network_proxies.select_next_some() => {
                    rooms_coordination.push(phase1);
                    rooms_rpc.insert(room_id, rpc_tx);
                }
            }

            // if let Ok(Some(srv_msg)) = self.from_service.try_next() {
            //     let daemon = service_messages.get_ref();
            //
            //     match srv_msg {
            //
            //     }
            // }
        }
    }
}
