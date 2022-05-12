use crate::negotiation::NegotiationChan;
use crate::ProtocolAgent;
use async_std::stream;
use async_std::stream::{interval, Interval};
use futures::channel::{mpsc, oneshot};
use futures::pin_mut;
use libp2p::PeerId;
use mpc_p2p::broadcast::{IncomingMessage, OutgoingResponse};
use mpc_p2p::{broadcast, MessageType, NetworkService};
use mpc_peerset::{PeersetHandle, RoomId};
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

pub(crate) struct Phase1Channel {
    id: RoomId,
    rx: Option<mpsc::Receiver<broadcast::IncomingMessage>>,
    on_local_rpc: oneshot::Receiver<(u16, ProtocolAgent)>,
    service: NetworkService,
}

impl Phase1Channel {
    pub fn new(
        room_id: RoomId,
        room_rx: mpsc::Receiver<broadcast::IncomingMessage>,
        service: NetworkService,
    ) -> (Self, oneshot::Sender<(u16, ProtocolAgent)>) {
        let (tx, rx) = oneshot::channel();
        (
            Self {
                id: room_id,
                rx: Some(room_rx),
                on_local_rpc: rx,
                service,
            },
            tx,
        )
    }
}

impl<T> Future for Phase1Channel {
    type Output = Phase1Msg;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.rx.as_mut().unwrap().try_next() {
            Ok(Some(msg)) => match msg.context.message_type {
                MessageType::Coordination => {
                    return Poll::Ready(Phase1Msg::FromRemote {
                        peer_id: msg.peer_id,
                        session_id: msg.context.session_id,
                        protocol_id: msg.context.protocol_id,
                        payload: msg.payload,
                        response_tx: msg.pending_response,
                        channel: Phase2Chan {
                            id: self.id.clone(),
                            rx: self.rx.take(),
                            timeout: stream::interval(Duration::from_secs(15)),
                            service: self.service.clone(),
                        },
                    });
                }
                MessageType::Computation => {
                    panic!("unexpected message type")
                }
            },
            _ => {}
        }

        if let Some((n, agent)) = self.on_local_rpc.try_recv().unwrap() {
            return Poll::Ready(Phase1Msg::FromLocal {
                id: self.id.clone(),
                n,
                agent,
                negotiation: NegotiationChan::new(
                    self.id.clone(),
                    self.rx.take().unwrap(),
                    n,
                    self.service.clone(),
                ),
            });
        }

        // Wake this task to be polled again.
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

pub(crate) enum Phase1Msg {
    FromRemote {
        peer_id: PeerId,
        protocol_id: u64,
        session_id: u64,
        payload: Vec<u8>,                               // for negotiation and stuff
        response_tx: oneshot::Sender<OutgoingResponse>, // respond if negotiation is fine
        channel: Phase2Chan,                            // listens after we respond
    },
    FromLocal {
        id: RoomId,
        n: u16,
        agent: ProtocolAgent,
        negotiation: NegotiationChan,
    },
}

struct Phase2Chan {
    id: RoomId,
    rx: Option<mpsc::Receiver<broadcast::IncomingMessage>>,
    timeout: Interval,
    service: NetworkService,
}

impl<T> Future for Phase2Chan {
    type Output = Phase2Msg;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.rx.as_mut().unwrap().try_next() {
            Ok(Some(msg)) => match msg.context.message_type {
                MessageType::Coordination => {
                    let peers: Vec<PeerId> = serde_ipld_dagcbor::from_slice(&*msg.payload).unwrap();
                    let (proxy, rx) = ReceiverProxy::new(
                        self.id.clone(),
                        self.rx.take().unwrap(),
                        self.service.clone(),
                        peers,
                    );
                    return Poll::Ready(Phase2Msg::Start {
                        room_id: self.id.clone(),
                        room_receiver: rx,
                        receiver_proxy: proxy,
                    });
                }
                MessageType::Computation => {
                    panic!("unexpected message type")
                }
            },
            _ => {}
        }

        // Remote peer gone offline or refused taking in us in set - returning to Phase 1
        if self.timeout.poll_next(cx).is_ready() {
            let (ch, tx) = Phase1Channel::new(
                self.id.clone(),
                self.rx.take().unwrap(),
                self.service.clone(),
            );
            return Poll::Ready(Phase2Msg::Abort(ch, tx));
        }

        // Wake this task to be polled again.
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

pub(crate) enum Phase2Msg {
    Start {
        room_id: RoomId,
        room_receiver: mpsc::Receiver<broadcast::IncomingMessage>,
        receiver_proxy: ReceiverProxy,
    },
    Abort(Phase1Channel, oneshot::Sender<(u16, ProtocolAgent)>),
}

pub(crate) struct ReceiverProxy {
    id: RoomId,
    rx: Option<mpsc::Receiver<broadcast::IncomingMessage>>,
    tx: mpsc::Sender<broadcast::IncomingMessage>,
    service: NetworkService,
    parties: HashSet<PeerId>,
}

impl ReceiverProxy {
    pub fn new(
        room_id: RoomId,
        room_rx: mpsc::Receiver<broadcast::IncomingMessage>,
        service: NetworkService,
        parties: impl Iterator<Item = PeerId>,
    ) -> (Self, mpsc::Receiver<IncomingMessage>) {
        let (tx, rx) = mpsc::channel(parties.len() - 1);
        (
            Self {
                id: room_id,
                rx: Some(room_rx),
                tx,
                service,
                parties,
            },
            rx,
        )
    }
}

impl<T> Future for ReceiverProxy {
    type Output = (Phase1Channel, oneshot::Sender<(u16, ProtocolAgent)>);

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.tx.is_closed() {
            return Poll::Ready(Phase1Channel::new(
                self.id.clone(),
                self.rx.take().unwrap(),
                self.service.clone(),
            ));
        }

        match self.rx.as_mut().unwrap().try_next() {
            Ok(Some(mut msg)) => {
                let fut = self.peerset.index_of_peer(0.into(), msg.peer_id);
                pin_mut!(fut);

                loop {
                    match fut.poll(cx) {
                        Poll::Ready(Ok(i)) => {
                            msg.peer_index = i;
                            self.tx.try_send(msg);
                            break;
                        }
                        Poll::Ready(Err(_)) => {
                            panic!("peerset unexpectedly returned error");
                        }
                        Poll::Pending => {}
                    }
                }
            }
            _ => {}
        }

        // Wake this task to be polled again.
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}
