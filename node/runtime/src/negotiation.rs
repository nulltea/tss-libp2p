use crate::coordination::{Phase1Channel, Phase2Msg, ReceiverProxy};
use crate::peerset::Peerset;
use crate::ProtocolAgent;
use async_std::stream;
use async_std::stream::Interval;
use futures::channel::{mpsc, oneshot};
use libp2p::PeerId;
use mpc_p2p::broadcast::IncomingMessage;
use mpc_p2p::{broadcast, MessageContext, MessageType, NetworkService};
use mpc_peerset::RoomId;
use std::borrow::BorrowMut;
use std::collections::{HashMap, HashSet};
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
    agent: Option<ProtocolAgent>,
    responses: Option<mpsc::Receiver<Result<(PeerId, Vec<u8>), broadcast::RequestFailure>>>,
    parties: HashSet<PeerId>,
}

impl NegotiationChan {
    pub fn new(
        room_id: RoomId,
        room_rx: mpsc::Receiver<broadcast::IncomingMessage>,
        n: u16,
        service: NetworkService,
    ) -> Self {
        Self {
            id: room_id,
            rx: Some(room_rx),
            n,
            timeout: stream::interval(Duration::from_secs(60)),
            service,
            agent: None,
            responses: None,
            parties: Default::default(),
        }
    }
}

impl<T> Future for NegotiationChan {
    type Output = NegotiationMsg;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(rx) = self.responses.borrow_mut() {
            match rx.try_next() {
                Ok(Some(Ok((peer_id, _)))) => {
                    self.parties.insert(peer_id);
                    if self.parties.len() == self.n {
                        let agent = self.agent.unwrap().unwrap_ref();
                        self.service.multicast_message(
                            &self.id,
                            self.parties.clone(),
                            MessageContext {
                                message_type: MessageType::Coordination,
                                session_id: agent.session_id().into(),
                                protocol_id: 0,
                            },
                            serde_ipld_dagcbor::to_vec(&self.parties).unwrap(),
                            None,
                        );
                        let parties = Peerset::new(self.parties.into_iter());
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
            let agent = self.agent.unwrap().unwrap_ref();
            let c = self.challenge.or(Some(vec![])).unwrap();
            let (tx, rx) = mpsc::channel((self.n - 1) as usize);
            self.service.broadcast_message(
                &self.id,
                MessageContext {
                    message_type: MessageType::Coordination,
                    session_id: agent.session_id().into(),
                    protocol_id: 0,
                },
                c,
                Some(tx),
            );
            self.responses.insert(rx);
        }

        // It took too long for peerset to be assembled  - reset to Phase 1.
        if self.timeout.poll_next(cx).is_ready() {
            let (ch, tx) = Phase1Channel::new(
                self.id.clone(),
                self.rx.take().unwrap(),
                self.service.clone(),
            );
            return Poll::Ready(NegotiationMsg::Abort(ch, tx));
        }

        // Wake this task to be polled again.
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

pub(crate) enum NegotiationMsg {
    Start {
        agent: ProtocolAgent,
        room_receiver: mpsc::Receiver<broadcast::IncomingMessage>,
        receiver_proxy: ReceiverProxy,
        parties: Peerset,
    },
    Abort(Phase1Channel, oneshot::Sender<(u16, ProtocolAgent)>),
}
