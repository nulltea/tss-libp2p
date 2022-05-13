use crate::negotiation::NegotiationChan;
use crate::network_proxy::ReceiverProxy;
use crate::peerset::Peerset;
use crate::ProtocolAgent;
use async_std::stream;
use async_std::stream::{interval, Interval};
use futures::channel::{mpsc, oneshot};
use futures::pin_mut;
use libp2p::PeerId;
use mpc_p2p::broadcast::{IncomingMessage, OutgoingResponse};
use mpc_p2p::{broadcast, MessageType, NetworkService, RoomId};
use mpc_peerset::{PeersetHandle, RoomId};
use std::collections::{HashMap, HashSet};
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
                            agent: None,
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
                negotiation: NegotiationChan::new(
                    self.id.clone(),
                    self.rx.take().unwrap(),
                    n,
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

struct Phase2Chan {
    id: RoomId,
    rx: Option<mpsc::Receiver<broadcast::IncomingMessage>>,
    timeout: Interval,
    service: NetworkService,
    agent: Option<ProtocolAgent>,
}

impl Phase2Chan {
    pub fn inject_agent(&mut self, a: ProtocolAgent) {
        self.agent.insert(a);
    }
}

impl<T> Future for Phase2Chan {
    type Output = Phase2Msg;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.rx.as_mut().unwrap().try_next() {
            Ok(Some(msg)) => match msg.context.message_type {
                MessageType::Coordination => {
                    let peers: Vec<PeerId> = serde_ipld_dagcbor::from_slice(&*msg.payload).unwrap();
                    let parties = Peerset::new(peers);
                    let (proxy, rx) = ReceiverProxy::new(
                        self.id.clone(),
                        self.rx.take().unwrap(),
                        self.service.clone(),
                        parties.c,
                    );
                    Poll::Ready(Phase2Msg::Start {
                        room_id: self.id.clone(),
                        room_receiver: rx,
                        receiver_proxy: proxy,
                        parties,
                    })
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
        parties: Peerset,
    },
    Abort(Phase1Channel, oneshot::Sender<(u16, ProtocolAgent)>),
}
