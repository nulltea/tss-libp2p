use crate::negotiation::{NegotiationChan, StartMsg};
use crate::network_proxy::ReceiverProxy;
use crate::peerset::Peerset;
use crate::ComputeAgentAsync;
use async_std::stream;
use async_std::stream::Interval;
use futures::channel::{mpsc, oneshot};
use futures::Stream;
use libp2p::PeerId;
use mpc_p2p::broadcast::OutgoingResponse;
use mpc_p2p::{broadcast, MessageType, NetworkService, RoomId};

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

pub(crate) struct Phase1Channel {
    id: RoomId,
    rx: Option<mpsc::Receiver<broadcast::IncomingMessage>>,
    on_local_rpc: oneshot::Receiver<LocalRpcMsg>,
    service: NetworkService,
}

impl Phase1Channel {
    pub fn new(
        room_id: RoomId,
        room_rx: mpsc::Receiver<broadcast::IncomingMessage>,
        service: NetworkService,
    ) -> (Self, oneshot::Sender<LocalRpcMsg>) {
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

impl Future for Phase1Channel {
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

        if let Some(LocalRpcMsg { n, args, agent }) = self.on_local_rpc.try_recv().unwrap() {
            return Poll::Ready(Phase1Msg::FromLocal {
                id: self.id.clone(),
                n,
                negotiation: NegotiationChan::new(
                    self.id.clone(),
                    self.rx.take().unwrap(),
                    n,
                    args,
                    self.service.clone(),
                    agent,
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
        negotiation: NegotiationChan,
    },
}

pub(crate) struct Phase2Chan {
    id: RoomId,
    rx: Option<mpsc::Receiver<broadcast::IncomingMessage>>,
    timeout: Interval,
    service: NetworkService,
}

impl Phase2Chan {
    pub fn abort(mut self) -> (RoomId, Phase1Channel, oneshot::Sender<LocalRpcMsg>) {
        let (ch, tx) = Phase1Channel::new(self.id.clone(), self.rx.take().unwrap(), self.service);
        return (self.id, ch, tx);
    }
}

impl Future for Phase2Chan {
    type Output = Phase2Msg;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.rx.as_mut().unwrap().try_next() {
            Ok(Some(msg)) => match msg.context.message_type {
                MessageType::Coordination => {
                    let start_msg =
                        StartMsg::from_bytes(msg.payload, self.service.local_peer_id()).unwrap();
                    let parties = start_msg.parties;
                    let (proxy, rx) = ReceiverProxy::new(
                        self.id.clone(),
                        self.rx.take().unwrap(),
                        self.service.clone(),
                        parties.clone(),
                    );
                    return Poll::Ready(Phase2Msg::Start {
                        room_id: self.id.clone(),
                        room_receiver: rx,
                        receiver_proxy: proxy,
                        parties,
                        init_body: start_msg.body,
                    });
                }
                MessageType::Computation => {
                    panic!("unexpected message type")
                }
            },
            _ => {}
        }

        // Remote peer gone offline or refused taking in us in set - returning to Phase 1
        if let Poll::Ready(Some(_)) = Stream::poll_next(Pin::new(&mut self.timeout), cx) {
            let (ch, tx) = Phase1Channel::new(
                self.id.clone(),
                self.rx.take().unwrap(),
                self.service.clone(),
            );
            return Poll::Ready(Phase2Msg::Abort(self.id.clone(), ch, tx));
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
        parties: Peerset,
        init_body: Vec<u8>,
    },
    Abort(RoomId, Phase1Channel, oneshot::Sender<LocalRpcMsg>),
}

pub(crate) struct LocalRpcMsg {
    n: u16,
    args: Vec<u8>,
    agent: Box<dyn ComputeAgentAsync>,
}
