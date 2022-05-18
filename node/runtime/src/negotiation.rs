use crate::coordination::Phase1Channel;
use crate::network_proxy::ReceiverProxy;
use crate::peerset::Peerset;
use crate::ComputeAgentAsync;
use async_std::stream;
use async_std::stream::Interval;
use futures::channel::{mpsc, oneshot};
use futures::Stream;
use libp2p::PeerId;

use mpc_p2p::{broadcast, MessageContext, MessageType, NetworkService, RoomId};
use std::borrow::BorrowMut;
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

pub(crate) struct NegotiationChan {
    id: RoomId,
    rx: Option<mpsc::Receiver<broadcast::IncomingMessage>>,
    n: u16,
    timeout: Interval,
    service: NetworkService,
    agent: Option<Box<dyn ComputeAgentAsync>>,
    responses: Option<mpsc::Receiver<Result<(PeerId, Vec<u8>), broadcast::RequestFailure>>>,
    peers: HashSet<PeerId>,
}

impl NegotiationChan {
    pub fn new(
        room_id: RoomId,
        room_rx: mpsc::Receiver<broadcast::IncomingMessage>,
        n: u16,
        service: NetworkService,
        agent: Box<dyn ComputeAgentAsync>,
    ) -> Self {
        Self {
            id: room_id,
            rx: Some(room_rx),
            n,
            timeout: stream::interval(Duration::from_secs(60)),
            service,
            agent: Some(agent),
            responses: None,
            peers: Default::default(),
        }
    }
}

impl Future for NegotiationChan {
    type Output = NegotiationMsg;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(rx) = self.responses.borrow_mut() {
            match rx.try_next() {
                Ok(Some(Ok((peer_id, _)))) => {
                    self.peers.insert(peer_id);
                    if self.peers.len() == self.n as usize {
                        let agent = self.agent.take().unwrap();
                        let peers = self.peers.clone();
                        let parties = Peerset::new(peers.clone().into_iter());

                        self.service.multicast_message(
                            &self.id,
                            peers.into_iter(),
                            MessageContext {
                                message_type: MessageType::Coordination,
                                session_id: agent.session_id().into(),
                                protocol_id: 0,
                            },
                            parties.to_bytes(),
                            None,
                        );
                        let (receiver_proxy, room_receiver) = ReceiverProxy::new(
                            self.id.clone(),
                            self.rx.take().unwrap(),
                            self.service.clone(),
                            parties.clone(),
                        );
                        return Poll::Ready(NegotiationMsg::Start {
                            agent: self.agent.take().unwrap(),
                            room_receiver,
                            receiver_proxy,
                            parties,
                        });
                    }
                }
                _ => {}
            }
        } else {
            let agent = self.agent.as_ref().unwrap();
            let (tx, rx) = mpsc::channel((self.n - 1) as usize);
            self.service.broadcast_message(
                &self.id,
                MessageContext {
                    message_type: MessageType::Coordination,
                    session_id: agent.session_id(),
                    protocol_id: agent.protocol_id(),
                },
                vec![],
                Some(tx),
            );
            self.responses.insert(rx);
        }

        // It took too long for peerset to be assembled  - reset to Phase 1.
        if Stream::poll_next(Pin::new(&mut self.timeout), cx).is_ready() {
            let (ch, tx) = Phase1Channel::new(
                self.id.clone(),
                self.rx.take().unwrap(),
                self.service.clone(),
            );
            return Poll::Ready(NegotiationMsg::Abort(self.id.clone(), ch, tx));
        }

        // Wake this task to be polled again.
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

pub(crate) enum NegotiationMsg {
    Start {
        agent: Box<dyn ComputeAgentAsync>,
        room_receiver: mpsc::Receiver<broadcast::IncomingMessage>,
        receiver_proxy: ReceiverProxy,
        parties: Peerset,
    },
    Abort(
        RoomId,
        Phase1Channel,
        oneshot::Sender<(u16, Box<dyn ComputeAgentAsync>)>,
    ),
}
