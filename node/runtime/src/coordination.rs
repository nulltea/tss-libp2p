use crate::ProtocolAgent;
use async_std::stream;
use async_std::stream::{interval, Interval};
use futures::channel::{mpsc, oneshot};
use libp2p::PeerId;
use mpc_p2p::broadcast::OutgoingResponse;
use mpc_p2p::{broadcast, MessageType};
use mpc_peerset::RoomId;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

pub(crate) struct Phase1Channel {
    pub id: RoomId,
    pub rx: Option<mpsc::Receiver<broadcast::IncomingMessage>>,
    pub on_local_rpc: oneshot::Receiver<(u16, ProtocolAgent)>,
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
                rx: self.rx.take().unwrap(),
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
        rx: NegotiationChan,
    },
}

struct Phase2Chan {
    id: RoomId,
    rx: Option<mpsc::Receiver<broadcast::IncomingMessage>>,
    timeout: Interval,
}

impl<T> Future for Phase2Chan {
    type Output = Phase2Msg;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.rx.as_mut().unwrap().try_next() {
            Ok(Some(msg)) => match msg.context.message_type {
                MessageType::Coordination => {
                    return Poll::Ready(Phase2Msg::Start {
                        room_id: self.id.clone(),
                        room_receiver: self.rx.take().unwrap(),
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
            let (tx, rx) = oneshot::channel();
            return Poll::Ready(Phase2Msg::Abort(
                Phase1Channel {
                    id: self.id.clone(),
                    rx: self.rx.take(),
                    on_local_rpc: rx,
                },
                tx,
            ));
        }

        // Wake this task to be polled again.
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

pub(crate) enum Phase2Msg {
    Start {
        room_id: RoomId,
        room_receiver: mpsc::Receiver<broadcast::IncomingMessage>, // hmm todo: maybe wrap it in the similar kind of struct (?)
                                                                   /* protocol_id:
                                                                   session_id: SessionId
                                                                   n: u16
                                                                    */
    },
    Abort(Phase1Channel, oneshot::Sender<(u16, ProtocolAgent)>),
}

struct NegotiationChan {
    id: RoomId,
    rx: Option<mpsc::Receiver<broadcast::IncomingMessage>>,
    n: u16,
    timeout: Interval,
}

impl<T> Future for NegotiationChan {
    type Output = Result<(), ()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.rx.as_mut().unwrap().try_next() {
            Ok(Some(msg)) => match msg.context.message_type {},
            _ => {}
        }

        // Remote peer gone offline or refused taking in us in set - returning to Phase 1
        if self.timeout.poll_next(cx).is_ready() {
            let (tx, rx) = oneshot::channel();
            return Poll::Ready(Err(()));
        }

        // Wake this task to be polled again.
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

pub(crate) struct ReceiverProxy<T> {
    id: RoomId,
    rx: Option<mpsc::Receiver<T>>,
    tx: mpsc::Sender<T>,
}

impl<T> Future for ReceiverProxy<T> {
    type Output = (Phase1Channel, oneshot::Sender<(u16, ProtocolAgent)>);

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.tx.is_closed() {
            let (tx, rx) = oneshot::channel();
            return Poll::Ready((
                Phase1Channel {
                    id: self.id.clone(),
                    rx: self.rx.take(),
                    on_local_rpc: rx,
                },
                tx,
            ));
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
