use crate::coordination::{Phase1Channel, ReceiverProxy};
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
    agent: ProtocolAgent,
    service: NetworkService,
    challenge: Option<Vec<u8>>,
    responses: Option<mpsc::Receiver<Result<(PeerId, Vec<u8>), broadcast::RequestFailure>>>,
    parties: HashSet<PeerId>,
}

impl NegotiationChan {
    pub fn new(
        room_id: RoomId,
        room_rx: mpsc::Receiver<broadcast::IncomingMessage>,
        agent: ProtocolAgent,
        n: u16,
        service: NetworkService,
    ) -> Self {
        Self {
            id: room_id,
            rx: Some(room_rx),
            n,
            timeout: stream::interval(Duration::from_secs(60)),
            agent,
            service,
            challenge: None,
            responses: None,
            parties: Default::default(),
        }
    }

    pub fn set_protocol_details(&mut self, protocol_id: u64, session_id: u64) {
        self.challenge.insert(c);
    }

    pub fn set_challenge(&mut self, c: Vec<u8>) {
        self.challenge.insert(c);
    }
}

impl<T> Future for NegotiationChan {
    type Output = Result<
        (ReceiverProxy, mpsc::Receiver<IncomingMessage>),
        (Phase1Channel, oneshot::Sender<(u16, ProtocolAgent)>),
    >;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(rx) = self.responses.borrow_mut() {
            match rx.try_next() {
                Ok(Some(Ok((peer_id, _)))) => {
                    self.parties.insert(peer_id);
                    if self.parties.len() == self.n {
                        self.service.broadcast_message(&self.id, self.parties.clone(), MessageContext {
                            message_type: MessageType::Coordination,
                            session_id: 0,
                            protocol_id: 0
                        }, vec![], ())
                        return Poll::Ready(Ok(ReceiverProxy::new(
                            self.id.clone(),
                            self.rx.take().unwrap(),
                            self.service.clone(),
                            self.parties.into_iter(),
                        )));
                    }
                }
                _ => {}
            }
        } else {
            // let c = self.challenge.or(Some(vec![])).unwrap();
            let (tx, rx) = mpsc::channel((self.n - 1) as usize);
            self.service
                .broadcast_message(&self.id, 0.into(), self.n, tx);
            self.responses.insert(rx);
        }

        // It took too long for peerset to be assembled  - reset to Phase 1.
        if self.timeout.poll_next(cx).is_ready() {
            return Poll::Ready(Err(Phase1Channel::new(
                self.id.clone(),
                self.rx.take().unwrap(),
                self.service.clone(),
            )));
        }

        // Wake this task to be polled again.
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}
