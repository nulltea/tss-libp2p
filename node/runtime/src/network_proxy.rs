use crate::coordination::{LocalRpcMsg, Phase1Channel};
use crate::peerset::Peerset;
use futures::channel::{mpsc, oneshot};
use mpc_p2p::broadcast::IncomingMessage;
use mpc_p2p::{broadcast, NetworkService, RoomId};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

pub(crate) struct ReceiverProxy {
    id: RoomId,
    rx: Option<mpsc::Receiver<IncomingMessage>>,
    tx: mpsc::Sender<broadcast::IncomingMessage>,
    service: NetworkService,
    parties: Peerset,
}

impl ReceiverProxy {
    pub fn new(
        room_id: RoomId,
        room_rx: mpsc::Receiver<IncomingMessage>,
        service: NetworkService,
        parties: Peerset,
    ) -> (Self, mpsc::Receiver<IncomingMessage>) {
        let (tx, rx) = mpsc::channel(parties.size() - 1);
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

impl Future for ReceiverProxy {
    type Output = (RoomId, Phase1Channel, oneshot::Sender<LocalRpcMsg>);

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.tx.is_closed() {
            let (ch, tx) = Phase1Channel::new(
                self.id.clone(),
                self.rx.take().unwrap(),
                self.service.clone(),
            );
            return Poll::Ready((self.id.clone(), ch, tx));
        }

        match self.rx.as_mut().unwrap().try_next() {
            Ok(Some(mut msg)) => match self.parties.index_of(&msg.peer_id) {
                Some(i) => {
                    msg.peer_index = i;
                    let _ = self.tx.try_send(msg);
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
