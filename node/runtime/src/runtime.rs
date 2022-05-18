use crate::coordination::Phase2Msg;
use crate::echo::{EchoGadget, EchoMessage, EchoResponse};
use crate::negotiation::NegotiationMsg;
use crate::peerset::Peerset;

use crate::{coordination, ComputeAgentAsync, MessageRouting, ProtocolAgentFactory};
use anyhow::anyhow;
use async_std::prelude::Stream;
use async_std::task;
use blake2::Digest;

use futures::channel::{mpsc, oneshot};
use futures::StreamExt;

use futures_util::stream::{FuturesOrdered, FuturesUnordered};
use futures_util::{select, FutureExt, SinkExt};
use log::{error, info};
use mpc_p2p::broadcast::{IncomingMessage, OutgoingResponse};
use mpc_p2p::{broadcast, MessageContext, MessageType, NetworkService, RoomId};

use std::collections::hash_map::Entry;
use std::collections::HashMap;

use std::future::Future;
use std::pin::Pin;

use std::task::{Context, Poll};

pub enum RuntimeMessage {
    JoinComputation(RoomId, u16, u64, oneshot::Sender<anyhow::Result<Vec<u8>>>),
}

#[derive(Clone)]
pub struct RuntimeService {
    to_runtime: mpsc::Sender<RuntimeMessage>,
}

impl RuntimeService {
    pub async fn join_computation(
        &mut self,
        room_id: RoomId,
        n: u16,
        protocol_id: u64,
        done: oneshot::Sender<anyhow::Result<Vec<u8>>>,
    ) {
        self.to_runtime
            .send(RuntimeMessage::JoinComputation(
                room_id,
                n,
                protocol_id,
                done,
            ))
            .await;
    }
}

pub struct RuntimeDaemon<TFactory> {
    network_service: NetworkService,
    rooms: HashMap<RoomId, mpsc::Receiver<broadcast::IncomingMessage>>,
    agents_factory: TFactory,
    from_service: mpsc::Receiver<RuntimeMessage>,
}

impl<TFactory: ProtocolAgentFactory + Send + Unpin> RuntimeDaemon<TFactory> {
    pub fn new(
        network_service: NetworkService,
        rooms: impl Iterator<Item = (RoomId, mpsc::Receiver<broadcast::IncomingMessage>)>,
        agents_factory: TFactory,
    ) -> (Self, RuntimeService) {
        let (tx, rx) = mpsc::channel(2);

        let worker = Self {
            network_service,
            rooms: rooms.collect(),
            from_service: rx,
            agents_factory,
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
                        RuntimeMessage::JoinComputation(room_id, n, protocol_id, done) => {
                            match rooms_rpc.entry(room_id) {
                                Entry::Occupied(e) => {
                                    let mut agent = match agents_factory.make(protocol_id) {
                                        Ok(a) => a,
                                        Err(_) => {
                                            done.send(Err(anyhow!("unknown protocol")));
                                            continue;
                                        }
                                    };
                                    let on_rpc = e.remove();

                                    if on_rpc.is_canceled() {
                                        done.send(Err(anyhow!("protocol is busy")));
                                    } else {
                                        agent.on_done(done);
                                        on_rpc.send((n, agent));
                                    }
                                }
                                Entry::Vacant(_) => {
                                    done.send(Err(anyhow!("protocol is busy")));
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
                            } => {
                                network_proxies.push(receiver_proxy);
                                let (echo, echo_tx) = EchoGadget::new(parties.size());
                                protocol_executions.push(echo.wrap_execution(ProtocolExecution::new(
                                    room_id,
                                    agent,
                                    network_service.clone(),
                                    parties,
                                    room_receiver,
                                    echo_tx,
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
                        negotiation,
                    } => {
                        match negotiation.await {
                            NegotiationMsg::Start {
                                agent,
                                room_receiver,
                                receiver_proxy,
                                parties,
                            } => {
                                network_proxies.push(receiver_proxy);
                                let (echo, echo_tx) = EchoGadget::new(n as usize);
                                protocol_executions.push(echo.wrap_execution(ProtocolExecution::new(
                                    id,
                                    agent,
                                    network_service.clone(),
                                    parties,
                                    room_receiver,
                                    echo_tx,
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

struct ProtocolExecution {
    state: Option<ProtocolExecState>,
}

struct ProtocolExecState {
    room_id: RoomId,
    session_id: u64,
    network_service: NetworkService,
    parties: Peerset,
    from_network: mpsc::Receiver<IncomingMessage>,
    to_protocol: mpsc::Sender<crate::IncomingMessage>,
    from_protocol: mpsc::Receiver<crate::OutgoingMessage>,
    echo_tx: mpsc::Sender<EchoMessage>,
    agent_future: Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>,
    pending_futures: FuturesOrdered<Pin<Box<dyn Future<Output = ()> + Send>>>,
    i: u16,
    n: u16,
}

impl ProtocolExecution {
    pub fn new(
        room_id: RoomId,
        agent: Box<dyn ComputeAgentAsync>,
        network_service: NetworkService,
        parties: Peerset,
        from_network: mpsc::Receiver<IncomingMessage>,
        echo_tx: mpsc::Sender<EchoMessage>,
    ) -> Self {
        let n = parties.size() as u16;
        let i = parties.index_of(&network_service.local_peer_id()).unwrap();
        let (to_protocol, from_runtime) = mpsc::channel((n - 1) as usize);
        let (to_runtime, from_protocol) = mpsc::channel((n - 1) as usize);

        let agent_future = agent.start(n, i, from_runtime, to_runtime);

        Self {
            state: Some(ProtocolExecState {
                room_id,
                session_id: 0,
                network_service,
                parties,
                from_network,
                to_protocol,
                from_protocol,
                echo_tx,
                agent_future,
                pending_futures: FuturesOrdered::new(),
                i,
                n,
            }),
        }
    }
}

impl Future for ProtocolExecution {
    type Output = crate::Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let ProtocolExecState {
            room_id,
            session_id,
            network_service,
            parties,
            mut from_network,
            mut to_protocol,
            mut from_protocol,
            mut echo_tx,
            mut agent_future,
            mut pending_futures,
            i,
            n,
        } = self.state.take().unwrap();

        loop {
            if let Poll::Pending = Stream::poll_next(Pin::new(&mut pending_futures).as_mut(), cx) {
                break;
            }
        }

        if let Poll::Ready(Some(message)) = Stream::poll_next(Pin::new(&mut from_network), cx) {
            info!("incoming message from {}", message.peer_id.to_base58());

            if message.is_broadcast {
                echo_tx
                    .try_send(EchoMessage {
                        sender: message.peer_index + 1,
                        payload: message.payload.clone(),
                        response: EchoResponse::Incoming(message.pending_response),
                    })
                    .map_err(|_e| anyhow!("echo send expected")); // todo: error handling
            } else {
                message
                    .pending_response
                    .send(OutgoingResponse {
                        result: Ok(vec![]),
                        sent_feedback: None,
                    })
                    .map_err(|_e| anyhow!("acknowledgement failed with error"));
                // todo: error handling
            }

            to_protocol.try_send(crate::IncomingMessage {
                from: message.peer_index + 1,
                to: if message.is_broadcast {
                    MessageRouting::Broadcast
                } else {
                    MessageRouting::PointToPoint(i + 1)
                },
                body: message.payload,
            });
        }

        if let Poll::Ready(Some(message)) = Stream::poll_next(Pin::new(&mut from_protocol), cx) {
            info!("outgoing message to {:?}", message.to);
            let _message_round = 1; // todo: index round somehow

            match message.to {
                MessageRouting::PointToPoint(remote_index) => {
                    let (res_tx, mut res_rx) = mpsc::channel(1);

                    pending_futures.push(
                        network_service
                            .clone()
                            .send_message_owned(
                                room_id.clone(),
                                parties[remote_index - 1],
                                MessageContext {
                                    message_type: MessageType::Computation,
                                    session_id,
                                    protocol_id: 0,
                                },
                                message.body,
                                res_tx,
                            )
                            .boxed(),
                    );

                    // todo: handle in same Future::poll
                    task::spawn(async move {
                        if let Err(e) = res_rx.select_next_some().await {
                            error!("party responded with error: {e}");
                        } else {
                            info!("party responded");
                        }
                    });
                }
                MessageRouting::Broadcast => {
                    let (res_tx, res_rx) = mpsc::channel((n - 1) as usize);

                    pending_futures.push(
                        network_service
                            .clone()
                            .multicast_message_owned(
                                room_id.clone(),
                                parties.clone().into_iter(),
                                MessageContext {
                                    message_type: MessageType::Coordination,
                                    session_id,
                                    protocol_id: 0,
                                },
                                message.body.clone(),
                                Some(res_tx),
                            )
                            .boxed(),
                    );

                    echo_tx.try_send(EchoMessage {
                        sender: i,
                        payload: message.body,
                        response: EchoResponse::Outgoing(res_rx),
                    });
                }
            }
        }

        match Future::poll(Pin::new(&mut agent_future), cx) {
            Poll::Ready(Ok(_)) => Poll::Ready(Ok(())),
            Poll::Ready(Err(e)) => Poll::Ready(Err(crate::Error::InternalError(e))),
            Poll::Pending => {
                self.state.insert(ProtocolExecState {
                    room_id,
                    session_id,
                    network_service,
                    parties,
                    from_network,
                    to_protocol,
                    from_protocol,
                    echo_tx,
                    agent_future,
                    pending_futures,
                    i,
                    n,
                });

                Poll::Pending
            }
        }
    }
}
