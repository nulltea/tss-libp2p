use blake2::{Blake2s256, Digest};
use futures::channel::{mpsc, oneshot};
use futures_util::{pin_mut, select, FutureExt, SinkExt, StreamExt};
use libp2p::PeerId;

use mpc_p2p::broadcast;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::future::Future;
use std::io::Write;

pub(crate) struct EchoGadget {
    r: u16,
    n: usize,
    msgs: BinaryHeap<EchoMessage>,
    rx: mpsc::Receiver<EchoMessage>,
}

impl EchoGadget {
    pub fn new(n: usize) -> (Self, mpsc::Sender<EchoMessage>) {
        let (tx, rx) = mpsc::channel(n);

        let gadget = EchoGadget {
            r: 0,
            n,
            msgs: Default::default(),
            rx,
        };

        (gadget, tx)
    }

    pub async fn wrap_execution(
        mut self,
        computation_fut: impl Future<Output = crate::Result<()>>,
    ) -> crate::Result<()> {
        let mut echo = Box::pin(self.proceed_round().fuse());
        let future = computation_fut.fuse();
        pin_mut!(future);

        loop {
            select! {
                echo_res = echo => match echo_res {
                    Ok(s) => {
                        echo = Box::pin(s.proceed_round().fuse());
                    },
                    Err(e) => {
                        return Err(e); // TODO: forgot to notify agent about error
                    }
                },
                _comp_res = future => {
                    return Ok(());
                }
            }
        }
    }

    async fn proceed_round(&mut self) -> crate::Result<&mut Self> {
        loop {
            let msg = self.rx.select_next_some().await;
            self.msgs.push(msg);
            if self.msgs.len() == self.n {
                break;
            }
        }

        let mut hasher = Blake2s256::new();
        let mut incoming_acks = vec![];
        let mut outgoing_resp_rx = None;

        while let Some(echo_msg) = self.msgs.pop() {
            hasher.write(&*echo_msg.payload);
            match echo_msg.response {
                EchoResponse::Incoming(tx) => incoming_acks.push(tx),
                EchoResponse::Outgoing(resp_rx) => {
                    outgoing_resp_rx.insert(resp_rx);
                }
            }
        }

        let mut outgoing_resp_rx = outgoing_resp_rx.expect("outgoing message was expected");

        let echo_hash = hasher.finalize().to_vec();
        for tx in incoming_acks.into_iter() {
            tx.send(broadcast::OutgoingResponse {
                result: Ok(echo_hash.clone()),
                sent_feedback: None,
            });
        }

        let mut echo_hashes = vec![];

        loop {
            echo_hashes.push(outgoing_resp_rx.select_next_some().await);

            if echo_hashes.len() == self.n - 1 {
                break; // todo: add timeout handling
            }
        }

        // there's a stupid bug below, todo: this index is not peer_index
        for (index, remote_echo) in echo_hashes.into_iter().enumerate() {
            match remote_echo {
                Ok((_peer_id, hash)) => {
                    if hash != echo_hash {
                        return Err(crate::Error::InconsistentEcho(index as u16));
                    }
                }
                Err(e) => {
                    return Err(crate::Error::EchoFailed(e));
                }
            }
        }

        self.r += 1;

        return Ok(self);
    }
}

pub(crate) struct EchoMessage {
    pub sender: u16,
    pub payload: Vec<u8>,
    pub response: EchoResponse,
}

impl Eq for EchoMessage {}

impl PartialEq<Self> for EchoMessage {
    fn eq(&self, other: &Self) -> bool {
        self.sender == other.sender
    }
}

impl PartialOrd<Self> for EchoMessage {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.sender.cmp(&other.sender))
    }
}

impl Ord for EchoMessage {
    fn cmp(&self, other: &Self) -> Ordering {
        self.sender.cmp(&other.sender)
    }
}

pub(crate) enum EchoResponse {
    Incoming(oneshot::Sender<broadcast::OutgoingResponse>),
    Outgoing(mpsc::Receiver<Result<(PeerId, Vec<u8>), broadcast::RequestFailure>>),
}
