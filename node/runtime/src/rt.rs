use crate::echo::{EchoGadget, EchoMessage, EchoResponse};
use crate::traits::ComputeAgent;
use crate::{Error, ProtocolAgent};
use anyhow::anyhow;
use async_std::prelude::Stream;
use async_std::task;
use blake2::Digest;
use futures::channel::mpsc::{Receiver, TryRecvError};
use futures::channel::{mpsc, oneshot};
use futures::{Sink, StreamExt};
use futures_util::stream::{Fuse, FuturesUnordered};
use futures_util::{future, pin_mut, select, FutureExt, SinkExt};
use log::{error, info};
use mpc_p2p::broadcast::{IncomingMessage, OutgoingResponse};
use mpc_p2p::{broadcast, NetworkService};
use mpc_peerset::RoomId;
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
    rooms: RwLock<HashMap<RoomId, mpsc::Receiver<broadcast::IncomingMessage>>>,
    from_service: mpsc::Receiver<RuntimeMessage>,
}

impl RuntimeDaemon {
    pub fn new(
        network_service: NetworkService,
        protocol_receivers: impl Iterator<Item = (RoomId, mpsc::Receiver<broadcast::IncomingMessage>)>,
    ) -> (Self, RuntimeService) {
        let (tx, rx) = mpsc::channel(2);

        let worker = Self {
            network_service,
            rooms: RwLock::new(protocol_receivers.collect()),
            from_service: rx,
        };

        let service = RuntimeService { to_runtime: tx };

        (worker, service)
    }

    pub async fn run(mut self) {
        let mut service_messages = self.fuse();
        let mut protocol_executions = FuturesUnordered::new();
        let mut network_proxies = FuturesUnordered::new();
        loop {
            let self_ref = service_messages.get_ref();
            select! {
                srv_msg = service_messages.select_next_some() => {
                    let daemon = service_messages.get_ref();

                    match srv_msg {
                        RuntimeMessage::JoinComputation(room_id, n, pa) => match pa {
                            ProtocolAgent::Keygen(agent) => {
                                let (proxy_tx, net_rx) = mpsc::channel((n - 1) as usize);
                                match daemon.proxy_incoming_messages(room_id, proxy_tx) {
                                    Ok(fut) => {
                                        network_proxies.push(fut);
                                        let (mut echo, echo_tx) = EchoGadget::new(n as usize);
                                        protocol_executions.push(echo.wrap_execution(
                                            join_computation(
                                                room_id,
                                                agent,
                                                daemon.network_service.clone(),
                                                net_rx,
                                                echo_tx,
                                                n,
                                            ),
                                        ));
                                    }
                                    Err(e) => {
                                        agent.done(Err(anyhow!("computation failed to start: {e}")));
                                    }
                                }
                            }
                        },
                    }
                },
                exec_res = protocol_executions.select_next_some() => match exec_res {
                    Ok(_) => {}
                    Err(e) => {error!("error during computation: {e}")}
                },
                (proto_id, net_rx) = network_proxies.select_next_some() => {
                    println!("proto_id returned");
                    service_messages.get_ref().protocol_receivers.write().unwrap().insert(proto_id, net_rx);
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

    fn proxy_incoming_messages(
        &self,
        room_id: RoomId,
        mut to: mpsc::Sender<IncomingMessage>,
    ) -> crate::Result<
        impl Future<
                Output = (
                    Cow<'static, str>,
                    mpsc::Receiver<broadcast::IncomingMessage>,
                ),
            > + 'static,
    > {
        let mut protocol_receivers = self.rooms.write().unwrap();

        // Mechanism bellow borrows protocol_receiver from hash_map for computation.
        // For protocols where concurrency is allowed (e.g. signing), 1-m receivers router is needed.
        let mut incoming_receiver = {
            match protocol_receivers.entry(room_id.clone()) {
                Entry::Occupied(e) => Ok(e.remove()),
                Entry::Vacant(_) => Err(crate::Error::Busy),
            }
        }?;

        let fut = ReceiverProxy {
            id: room_id,
            rx: Some(incoming_receiver),
            tx: to,
        };

        Ok(fut)
    }
}

async fn join_computation<CA: ?Sized>(
    room_id: RoomId,
    mut agent: Box<CA>,
    network_service: NetworkService,
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
    network_service
        .request_computation(&room_id, session_id, n)
        .await;

    let i = network_service
        .index_in_session(session_id, network_service.local_peer_id())
        .await
        .unwrap();

    let state_machine = agent.construct_state(i + 1, n);
    let (incoming, outgoing) = state_replication(
        room_id,
        network_service,
        session_id.into(),
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
    network_service: NetworkService,
    session_id: u64,
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
        (network_service.clone(), echo_tx.clone(), room_id.clone()),
        move |(network_service, mut echo_out, room_id), message: Msg<M>| async move {
            info!("outgoing message to {:?}", message);
            let payload = serde_ipld_dagcbor::to_vec(&message.body).map_err(|e| anyhow!("{e}"))?;
            let message_round = 1; // todo: index round somehow

            if let Some(receiver_index) = message.receiver {
                let (res_tx, mut res_rx) = mpsc::channel(1);

                network_service
                    .send_message(
                        session_id,
                        room_id.clone(),
                        message_round,
                        receiver_index - 1,
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
                    .broadcast_message(
                        session_id,
                        room_id.clone(),
                        message_round,
                        payload.clone(),
                        res_tx,
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

            Ok::<_, anyhow::Error>((network_service, echo_out, room_id))
        },
    );

    (incoming, outgoing)
}

struct ReceiverProxy<T> {
    id: RoomId,
    rx: Option<mpsc::Receiver<T>>,
    tx: mpsc::Sender<T>,
}

impl<T> Future for ReceiverProxy<T> {
    type Output = (RoomId, mpsc::Receiver<T>);

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.tx.is_closed() {
            return Poll::Ready((self.id.clone(), self.rx.take().unwrap()));
        }

        match self.rx.as_mut().unwrap().try_next() {
            Ok(Some(msg)) => {
                self.tx.try_send(msg);
            }
            _ => {}
        }

        // Wake this task to be polled again.
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

impl Stream for RuntimeDaemon {
    type Item = RuntimeMessage;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.from_service.poll_next_unpin(cx)
    }
}

struct CoordinationChannel {
    id: RoomId,
    rx: Option<mpsc::Receiver<broadcast::IncomingMessage>>,
    pull_back: oneshot::Receiver<(u16, ProtocolAgent)>,
}

impl<T> Future for CoordinationChannel {
    type Output = CoordinationPhase1;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.rx.as_mut().unwrap().try_next() {
            Ok(Some(msg)) => {
                /// todo: somehow check that we actually received `join_computation` request
                return Poll::Ready(CoordinationPhase1::Remote {
                    peer_id: msg.peer_index, // todo: should be PeerId !!!
                    payload: msg.payload,
                    response_tx: msg.pending_response,
                    channel: JoinChannel {
                        id: self.id.clone(),
                        rx: self.rx.take(),
                    },
                });
            }
            _ => {}
        }

        if let Some((n, agent)) = self.pull_back.try_recv().unwrap() {
            return Poll::Ready(CoordinationPhase1::Local {
                id: self.id.clone(),
                n,
                rx: self.rx.take(),
            });
        }

        // Wake this task to be polled again.
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}
// dude, these are session types, todo: try using dedicated library
enum CoordinationPhase1 {
    Remote {
        peer_id: u16,                                   // todo: PeerId
        payload: Vec<u8>, // for negotiation and stuff, todo: should contain protocol_id explicitly
        response_tx: oneshot::Sender<OutgoingResponse>, // responds if negotiation is fine
        channel: JoinChannel, // listens after it responds
    },
    Local {
        id: RoomId,
        n: u16,
        rx: Option<mpsc::Receiver<broadcast::IncomingMessage>>,
    },
}

/// todo: this struct is the same as `CoordinationChannel` how can we implement them differently (?)
struct JoinChannel {
    id: RoomId,
    rx: Option<mpsc::Receiver<broadcast::IncomingMessage>>,
}

enum CoordinationPhase2 {
    Start {
        room_id: RoomId,
        room_receiver: mpsc::Receiver<broadcast::IncomingMessage>, // hmm todo: maybe wrap it in the similar kind of struct (?)
                                                                   /* protocol_id:
                                                                   session_id: SessionId
                                                                   n: u16
                                                                    */
    },
    Abort(CoordinationChannel),
}

impl<T> Future for JoinChannel {
    type Output = CoordinationPhase2;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.rx.as_mut().unwrap().try_next() {
            Ok(Some(msg)) => {
                if true
                /* todo: some check logic here */
                {
                    return Poll::Ready(CoordinationPhase2::Start {
                        room_id: self.id.clone(),
                        room_receiver: self.rx.take().unwrap(),
                    });
                } else {
                    return Poll::Ready(CoordinationPhase2::Abort(CoordinationChannel {
                        id: self.id.clone(),
                        rx: self.rx.take(),
                    }));
                }
            }
            _ => {}
        }

        // Wake this task to be polled again.
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}
