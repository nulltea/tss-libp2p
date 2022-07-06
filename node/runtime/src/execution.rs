use crate::echo::{EchoMessage, EchoResponse};
use crate::peerset::Peerset;
use crate::{ComputeAgentAsync, MessageRouting, PeersetCacher, PeersetMsg, PersistentCacher};
use anyhow::anyhow;
use async_std::task;
use futures::channel::mpsc;
use futures::Stream;
use futures_util::stream::FuturesOrdered;
use futures_util::{FutureExt, StreamExt};
use libp2p::PeerId;
use log::{error, info};
use mpc_p2p::broadcast::OutgoingResponse;
use mpc_p2p::{broadcast, MessageContext, MessageType, NetworkService, RoomId};
use std::borrow::BorrowMut;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

pub(crate) struct ProtocolExecution {
    state: Option<ProtocolExecState>,
}

struct ProtocolExecState {
    room_id: RoomId,
    local_peer_id: PeerId,
    protocol_id: u64,
    session_id: u64,
    network_service: NetworkService,
    parties: Peerset,
    peerset_rx: mpsc::Receiver<PeersetMsg>,
    from_network: mpsc::Receiver<broadcast::IncomingMessage>,
    to_protocol: async_channel::Sender<crate::IncomingMessage>,
    from_protocol: async_channel::Receiver<crate::OutgoingMessage>,
    echo_tx: mpsc::Sender<EchoMessage>,
    agent_future: Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>,
    pending_futures: FuturesOrdered<Pin<Box<dyn Future<Output = ()> + Send>>>,
    cacher: PersistentCacher,
    i: u16,
    n: u16,
}

impl ProtocolExecution {
    pub fn new(
        room_id: RoomId,
        args: Vec<u8>,
        agent: Box<dyn ComputeAgentAsync>,
        network_service: NetworkService,
        parties: Peerset,
        peerset_rx: mpsc::Receiver<PeersetMsg>,
        cacher: PersistentCacher,
        from_network: mpsc::Receiver<broadcast::IncomingMessage>,
        echo_tx: mpsc::Sender<EchoMessage>,
    ) -> Self {
        let n = parties.size() as u16;
        let i = parties.index_of(parties.local_peer_id()).unwrap();
        let protocol_id = agent.protocol_id();
        let (to_protocol, from_runtime) = async_channel::bounded((n - 1) as usize);
        let (to_runtime, from_protocol) = async_channel::bounded((n - 1) as usize);

        let agent_future = agent.start(parties.clone(), args, from_runtime, to_runtime);

        Self {
            state: Some(ProtocolExecState {
                room_id,
                local_peer_id: network_service.local_peer_id(),
                protocol_id,
                session_id: 0,
                network_service,
                parties,
                peerset_rx,
                from_network,
                to_protocol,
                from_protocol,
                echo_tx,
                agent_future,
                pending_futures: FuturesOrdered::new(),
                cacher,
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
            local_peer_id,
            protocol_id,
            session_id,
            network_service,
            parties,
            peerset_rx: mut from_peerset,
            mut from_network,
            mut to_protocol,
            mut from_protocol,
            mut echo_tx,
            mut agent_future,
            mut pending_futures,
            mut cacher,
            i,
            n,
        } = self.state.take().unwrap();

        if let Poll::Ready(Some(message)) = Stream::poll_next(Pin::new(&mut from_peerset), cx) {
            match message {
                PeersetMsg::ReadFromCache(tx) => {
                    let _ = tx.send(cacher.read_peerset(&room_id));
                }
                PeersetMsg::WriteToCache(peerset, tx) => {
                    let _ = tx.send(cacher.write_peerset(&room_id, peerset));
                }
            }
        }

        if let Poll::Ready(Some(message)) = Stream::poll_next(Pin::new(&mut from_protocol), cx) {
            info!("outgoing message to {:?}", message.to,);

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
                                    protocol_id,
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

                    if let Some(tx) = message.sent {
                        let _ = tx.send(());
                    }
                }
                MessageRouting::Broadcast => {
                    let (res_tx, res_rx) = mpsc::channel((n - 1) as usize);

                    pending_futures.push(
                        network_service
                            .clone()
                            .multicast_message_owned(
                                room_id.clone(),
                                parties.clone().remotes_iter(),
                                MessageContext {
                                    message_type: MessageType::Coordination,
                                    session_id,
                                    protocol_id,
                                },
                                message.body.clone(),
                                Some(res_tx),
                            )
                            .boxed(),
                    );

                    echo_tx.try_send(EchoMessage {
                        sender: i + 1,
                        payload: message.body,
                        response: EchoResponse::Outgoing(res_rx),
                    });
                }
            }
        }

        loop {
            if let Poll::Ready(None) =
                Stream::poll_next(Pin::new(&mut pending_futures).as_mut(), cx)
            {
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

            info!("going to send outgoing msgs");
        }

        match Future::poll(Pin::new(&mut agent_future), cx) {
            Poll::Ready(Ok(_)) => Poll::Ready(Ok(())),
            Poll::Ready(Err(e)) => {
                error!("error: {e}");
                Poll::Ready(Err(crate::Error::InternalError(e)))
            }
            Poll::Pending => {
                self.state.insert(ProtocolExecState {
                    room_id,
                    local_peer_id,
                    protocol_id,
                    session_id,
                    network_service,
                    parties,
                    peerset_rx: from_peerset,
                    from_network,
                    to_protocol,
                    from_protocol,
                    echo_tx,
                    agent_future,
                    pending_futures,
                    cacher,
                    i,
                    n,
                });

                // Wake this task to be polled again.
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }
}
