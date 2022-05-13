use crate::coordination::{Phase1Channel, Phase1Msg, Phase2Msg, ReceiverProxy};
use crate::echo::{EchoGadget, EchoMessage, EchoResponse};
use crate::negotiation::{NegotiationChan, NegotiationMsg};
use crate::peerset::Peerset;
use crate::traits::ComputeAgent;
use crate::{coordination, Error, ProtocolAgent};
use anyhow::anyhow;
use async_std::prelude::Stream;
use async_std::task;
use blake2::Digest;
use futures::channel::mpsc::{Receiver, TryRecvError};
use futures::channel::oneshot::Sender;
use futures::channel::{mpsc, oneshot};
use futures::{Sink, StreamExt};
use futures_util::stream::{Fuse, FuturesUnordered};
use futures_util::{future, pin_mut, select, FutureExt, SinkExt};
use log::{error, info};
use mpc_p2p::broadcast::{IncomingMessage, OutgoingResponse};
use mpc_p2p::{broadcast, MessageContext, MessageType, NetworkService, RoomId};
use mpc_peerset::RoomId;
use round_based::async_runtime::Error::Send;
use round_based::{AsyncProtocol, Msg, StateMachine};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::borrow::{Borrow, BorrowMut, Cow};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::fmt::Debug;
use std::fmt::Display;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, LockResult, Mutex, RwLock};
use std::task::{Context, Poll};

pub enum RuntimeMessage {
    JoinComputation(RoomId, u16, ProtocolAgent),
}

#[derive(Clone)]
pub struct RuntimeService {
    to_runtime: mpsc::Sender<RuntimeMessage>,
}

impl RuntimeService {
    pub async fn join_computation(&mut self, room_id: RoomId, n: u16, agent: ProtocolAgent) {
        self.to_runtime
            .send(RuntimeMessage::JoinComputation(room_id, n, agent))
            .await;
    }
}

pub struct RuntimeDaemon {
    network_service: NetworkService,
    rooms: HashMap<RoomId, mpsc::Receiver<broadcast::IncomingMessage>>,
    protocols: HashMap<u64, ProtocolAgent>,
    from_service: mpsc::Receiver<RuntimeMessage>,
}

impl RuntimeDaemon {
    pub fn new(
        network_service: NetworkService,
        rooms: impl Iterator<Item = (RoomId, mpsc::Receiver<broadcast::IncomingMessage>)>,
        protocols: impl Iterator<Item = (u64, ProtocolAgent)>,
    ) -> (Self, RuntimeService) {
        let (tx, rx) = mpsc::channel(2);

        let worker = Self {
            network_service,
            rooms: rooms.collect(),
            from_service: rx,
            protocols: protocols.collect(),
        };

        let service = RuntimeService { to_runtime: tx };

        (worker, service)
    }

    pub async fn run(mut self) {
        let mut service_messages = self.fuse();
        let mut protocol_executions = FuturesUnordered::new();
        let mut network_proxies = FuturesUnordered::new();

        let mut rooms_coordination = FuturesUnordered::new();
        let mut rooms_rpc = HashMap::default();

        for (room_id, rx) in self.rooms.into_iter() {
            let (ch, tx) =
                coordination::Phase1Channel::new(room_id.clone(), rx, self.network_service.clone());
            rooms_coordination.push(ch);
            rooms_rpc.insert(room_id, tx);
        }

        let daemon = service_messages.get_ref();

        // loop {
        //     match network_proxies.select_next_some().await {}
        // }

        loop {
            select! {
                srv_msg = service_messages.select_next_some() => {
                    let daemon = service_messages.get_ref();

                    match srv_msg {
                        RuntimeMessage::JoinComputation(room_id, n, pa) => {
                            match rooms_rpc.entry(room_id) {
                                Entry::Occupied(e) => {
                                    if e.remove().send((n, pa)).is_err() {
                                        agent.done(Err(anyhow!("protocol is busy")));
                                    }
                                }
                                Entry::Vacant(_) => {
                                    agent.done(Err(anyhow!("protocol is busy")));
                                }
                            }
                        },
                    }
                },
                coord_msg = rooms_coordination.select_next_some() => match coord_msg {
                    coordination::Phase1Msg::FromRemote {
                        peer_id,
                        protocol_id,
                        session_id,
                        payload,
                        response_tx,
                        channel,
                    } => {
                        let agent = match self.protocols.get_mut(&protocol_id) {
                            Some(pa) => pa.clone_inner(),
                            None => {
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
                                let (mut echo, echo_tx) = EchoGadget::new(n as usize);
                                protocol_executions.push(echo.wrap_execution(join_computation(
                                    room_id,
                                    agent,
                                    daemon.network_service.clone(),
                                    parties,
                                    room_receiver,
                                    echo_tx,
                                    n,
                                )));
                            }
                            Phase2Msg::Abort(ch, tx) => {
                                rooms_rpc.entry(room_id.clone()).and_modify(|e| *e = tx);
                                rooms_coordination.push(ch);
                            }
                        }
                    }
                    coordination::Phase1Msg::FromLocal {
                        id,
                        n,
                        mut negotiation,
                    } => {
                        let (agent, net_rx) = match negotiation.await {
                            NegotiationMsg::Start {
                                agent,
                                room_receiver,
                                receiver_proxy,
                                parties,
                            } => {
                                network_proxies.push(receiver_proxy);
                                (agent, room_receiver)
                            }
                            Err((phase1, rpc_tx)) => {
                                rooms_coordination.push(phase1);
                                rooms_rpc.insert(id.clone(), rpc_tx);
                                continue;
                            }
                        };

                        let (mut echo, echo_tx) = EchoGadget::new(n as usize);
                        protocol_executions.push(echo.wrap_execution(join_computation(
                            room_id,
                            agent.unwrap(),
                            daemon.network_service.clone(),
                            parties,
                            net_rx,
                            echo_tx,
                            n,
                        )));
                    }
                }
                exec_res = protocol_executions.select_next_some() => match exec_res {
                    Ok(_) => {}
                    Err(e) => {error!("error during computation: {e}")}
                },
                (phase1, rpc_tx) = network_proxies.select_next_some() => {
                    rooms_coordination.push(phase1);
                    rooms_rpc.insert(id.clone(), rpc_tx);
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

async fn join_computation<CA: ?Sized>(
    room_id: RoomId,
    mut agent: Box<CA>,
    network_service: NetworkService,
    parties: Peerset,
    net_rx: mpsc::Receiver<IncomingMessage>,
    echo_tx: mpsc::Sender<EchoMessage>,
    n: u16,
) where
    CA: ComputeAgent,
    CA::StateMachine: Send + 'static,
    <<CA as ComputeAgent>::StateMachine as StateMachine>::Err: Send + Display,
    <<CA as ComputeAgent>::StateMachine as StateMachine>::MessageBody:
        Serialize + DeserializeOwned + Debug,
{
    let session_id = agent.session_id();

    let state_machine = agent.construct_state(i + 1, n);
    let (incoming, outgoing) = state_replication(
        room_id,
        session_id.into(),
        network_service,
        parties,
        net_rx,
        echo_tx,
        i,
        n,
    );
    let incoming = incoming.fuse();
    pin_mut!(incoming, outgoing);

    agent.done(
        AsyncProtocol::new(state_machine, incoming, outgoing)
            .run()
            .await
            .map_err(|e| anyhow!("protocol execution terminated with error: {e}")),
    );
}

fn state_replication<M>(
    room_id: RoomId,
    session_id: u64,
    network_service: NetworkService,
    parties: Peerset,
    net_rx: mpsc::Receiver<IncomingMessage>,
    echo_tx: mpsc::Sender<EchoMessage>,
    i: u16,
    n: u16,
) -> (
    impl Stream<Item = Result<Msg<M>, anyhow::Error>>,
    impl Sink<Msg<M>, Error = anyhow::Error>,
)
where
    M: Serialize + DeserializeOwned + Debug,
{
    let mut echo_in = echo_tx.clone();

    let incoming = net_rx.map(move |message: broadcast::IncomingMessage| {
        let body: M = serde_ipld_dagcbor::from_slice(&*message.payload)
            .map_err(|e| anyhow!("decode terminated with err: {e}"))?;
        info!(
            "incoming message from {} => {:?}",
            message.peer_index + 1,
            body
        );

        if message.is_broadcast {
            echo_in
                .try_send(EchoMessage {
                    sender: message.peer_index + 1,
                    payload: message.payload.clone(),
                    response: EchoResponse::Incoming(message.pending_response),
                })
                .map_err(|_e| anyhow!("echo send expected"))?
        } else {
            message
                .pending_response
                .send(OutgoingResponse {
                    result: Ok(vec![]),
                    sent_feedback: None,
                })
                .map_err(|_e| anyhow!("acknowledgement failed with error"))?;
        }

        Ok::<_, anyhow::Error>(Msg {
            sender: message.peer_index + 1,
            receiver: if message.is_broadcast {
                None
            } else {
                Some(i + 1)
            },
            body,
        })
    });

    let outgoing = futures::sink::unfold(
        (
            network_service.clone(),
            echo_tx.clone(),
            room_id.clone(),
            parties,
        ),
        move |(network_service, mut echo_out, room_id, parties), message: Msg<M>| async move {
            info!("outgoing message to {:?}", message);
            let payload = serde_ipld_dagcbor::to_vec(&message.body).map_err(|e| anyhow!("{e}"))?;
            let message_round = 1; // todo: index round somehow

            if let Some(receiver_index) = message.receiver {
                let (res_tx, mut res_rx) = mpsc::channel(1);

                network_service
                    .send_message(
                        &room_id,
                        parties[receiver_index - 1],
                        MessageContext {
                            message_type: MessageType::Computation,
                            session_id,
                            protocol_id: 0,
                        },
                        payload,
                        res_tx,
                    )
                    .await;

                // todo: handle in same Future::poll
                task::spawn(async move {
                    if let Err(e) = res_rx.select_next_some().await {
                        error!("party responded with error: {e}");
                    } else {
                        info!("party responded");
                    }
                });
            } else {
                let (res_tx, res_rx) = mpsc::channel((n - 1) as usize);

                network_service
                    .multicast_message(
                        &room_id,
                        parties.clone().into_iter(),
                        MessageContext {
                            message_type: MessageType::Coordination,
                            session_id,
                            protocol_id: 0,
                        },
                        payload.clone(),
                        Some(res_tx),
                    )
                    .await;

                echo_out
                    .send(EchoMessage {
                        sender: message.sender,
                        payload,
                        response: EchoResponse::Outgoing(res_rx),
                    })
                    .await;
            }

            Ok::<_, anyhow::Error>((network_service, echo_out, room_id, parties))
        },
    );

    (incoming, outgoing)
}

impl Stream for RuntimeDaemon {
    type Item = RuntimeMessage;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.from_service.poll_next_unpin(cx)
    }
}
