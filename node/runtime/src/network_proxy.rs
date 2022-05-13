use crate::coordination::Phase1Channel;
use crate::peerset::Peerset;
use crate::ProtocolAgent;
use futures::channel::{mpsc, oneshot};
use futures_util::pin_mut;
use libp2p::PeerId;
use mpc_p2p::broadcast::IncomingMessage;
use mpc_p2p::{broadcast, NetworkService};
use mpc_peerset::RoomId;
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

pub(crate) struct ReceiverProxy {
    id: RoomId,
    rx: Option<mpsc::Receiver<broadcast::IncomingMessage>>,
    tx: mpsc::Sender<broadcast::IncomingMessage>,
    service: NetworkService,
    parties: Peerset,
}

impl ReceiverProxy {
    pub fn new(
        room_id: RoomId,
        room_rx: mpsc::Receiver<broadcast::IncomingMessage>,
        service: NetworkService,
        parties: Peerset,
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
            Ok(Some(mut msg)) => match self.parties.index_of(&msg.peer_id) {
                Some(i) => {
                    msg.peer_index = i;
                    self.tx.try_send(msg);
                }
                None => {
                    panic!("received message from unknown peer");
                }
            },
            _ => {}
        }

        // Wake this task to be polled again.
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}
