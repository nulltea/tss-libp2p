use crate::coordination::{LocalRpcMsg, Phase1Channel};
use crate::network_proxy::ReceiverProxy;
use crate::peerset::Peerset;
use crate::{ComputeAgentAsync, PeersetMsg};
use async_std::stream;
use async_std::stream::Interval;
use futures::channel::{mpsc, oneshot};
use futures::Stream;
use futures_util::stream::FuturesOrdered;
use futures_util::FutureExt;
use libp2p::PeerId;
use log::info;
use mpc_p2p::{broadcast, MessageContext, MessageType, NetworkService, RoomId};
use std::borrow::BorrowMut;
use std::collections::HashSet;
use std::future::Future;
use std::io::{BufReader, BufWriter, Read, Write};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use std::{io, iter};

pub(crate) struct NegotiationChan {
    rx: Option<mpsc::Receiver<broadcast::IncomingMessage>>,
    timeout: Interval,
    agent: Option<Box<dyn ComputeAgentAsync>>,
    state: Option<NegotiationState>,
}

struct NegotiationState {
    id: RoomId,
    n: u16,
    args: Vec<u8>,
    service: NetworkService,
    peers: HashSet<PeerId>,
    responses: Option<mpsc::Receiver<Result<(PeerId, Vec<u8>), broadcast::RequestFailure>>>,
    pending_futures: FuturesOrdered<Pin<Box<dyn Future<Output = ()> + Send>>>,
    on_done: oneshot::Sender<anyhow::Result<Vec<u8>>>,
}

impl NegotiationChan {
    pub fn new(
        room_id: RoomId,
        room_rx: mpsc::Receiver<broadcast::IncomingMessage>,
        n: u16,
        args: Vec<u8>,
        service: NetworkService,
        agent: Box<dyn ComputeAgentAsync>,
        on_done: oneshot::Sender<anyhow::Result<Vec<u8>>>,
    ) -> Self {
        let local_peer_id = service.local_peer_id();
        Self {
            rx: Some(room_rx),
            timeout: stream::interval(Duration::from_secs(15)),
            agent: Some(agent),
            state: Some(NegotiationState {
                id: room_id,
                n,
                args,
                service,
                peers: iter::once(local_peer_id).collect(),
                responses: None,
                pending_futures: Default::default(),
                on_done,
            }),
        }
    }
}

impl Future for NegotiationChan {
    type Output = NegotiationMsg;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let NegotiationState {
            id,
            n,
            args,
            service,
            mut peers,
            mut responses,
            mut pending_futures,
            on_done,
        } = self.state.take().unwrap();

        loop {
            if let Poll::Ready(None) =
                Stream::poll_next(Pin::new(&mut pending_futures).as_mut(), cx)
            {
                break;
            }
        }

        if let Some(rx) = responses.borrow_mut() {
            match rx.try_next() {
                Ok(Some(Ok((peer_id, _)))) => {
                    peers.insert(peer_id);
                    if peers.len() == n as usize {
                        let agent = self.agent.take().unwrap();
                        let peers_iter = peers.clone().into_iter();
                        let (parties, peerset_rx) =
                            Peerset::new(peers_iter, service.local_peer_id());
                        let start_msg = StartMsg {
                            parties: parties.clone(),
                            body: args.clone(),
                        };
                        pending_futures.push(
                            service
                                .clone()
                                .multicast_message_owned(
                                    id.clone(),
                                    peers.clone().into_iter(),
                                    MessageContext {
                                        message_type: MessageType::Coordination,
                                        session_id: agent.session_id().into(),
                                        protocol_id: agent.protocol_id(),
                                    },
                                    start_msg.to_bytes().unwrap(),
                                    None,
                                )
                                .boxed(),
                        );

                        loop {
                            if let Poll::Ready(None) =
                                Stream::poll_next(Pin::new(&mut pending_futures).as_mut(), cx)
                            {
                                break;
                            }
                        }

                        let (receiver_proxy, room_receiver) = ReceiverProxy::new(
                            id.clone(),
                            self.rx.take().unwrap(),
                            service.clone(),
                            parties.clone(),
                        );
                        return Poll::Ready(NegotiationMsg::Start {
                            agent,
                            on_done,
                            room_receiver,
                            receiver_proxy,
                            parties,
                            peerset_rx,
                            args,
                        });
                    }
                }
                _ => {}
            }
        } else {
            let agent = self.agent.as_ref().unwrap();
            let (tx, rx) = mpsc::channel((n - 1) as usize);
            pending_futures.push(
                service
                    .clone()
                    .broadcast_message_owned(
                        id.clone(),
                        MessageContext {
                            message_type: MessageType::Coordination,
                            session_id: agent.session_id(),
                            protocol_id: agent.protocol_id(),
                        },
                        vec![],
                        Some(tx),
                    )
                    .boxed(),
            );
            let _ = responses.insert(rx);
        }

        // It took too long for peerset to be assembled  - reset to Phase 1.
        if let Poll::Ready(Some(())) = Stream::poll_next(Pin::new(&mut self.timeout), cx) {
            let (ch, tx) = Phase1Channel::new(id.clone(), self.rx.take().unwrap(), service.clone());
            return Poll::Ready(NegotiationMsg::Abort(id.clone(), ch, tx));
        }

        let _ = self.state.insert(NegotiationState {
            id,
            n,
            args,
            service,
            peers,
            responses,
            pending_futures,
            on_done,
        });

        // Wake this task to be polled again.
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

pub(crate) enum NegotiationMsg {
    Start {
        agent: Box<dyn ComputeAgentAsync>,
        on_done: oneshot::Sender<anyhow::Result<Vec<u8>>>,
        room_receiver: mpsc::Receiver<broadcast::IncomingMessage>,
        receiver_proxy: ReceiverProxy,
        parties: Peerset,
        peerset_rx: mpsc::Receiver<PeersetMsg>,
        args: Vec<u8>,
    },
    Abort(RoomId, Phase1Channel, oneshot::Sender<LocalRpcMsg>),
}

pub(crate) struct StartMsg {
    pub parties: Peerset,
    pub body: Vec<u8>,
}

impl StartMsg {
    pub(crate) fn from_bytes(
        b: &[u8],
        local_peer_id: PeerId,
    ) -> io::Result<(Self, mpsc::Receiver<PeersetMsg>)> {
        let mut io = BufReader::new(b);

        // Read the peerset payload length.
        let peerset_len = unsigned_varint::io::read_usize(&mut io)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

        let mut peerset_buffer = vec![0; peerset_len];
        io.read_exact(&mut peerset_buffer)?;

        // Read the body payload length.
        let length = unsigned_varint::io::read_usize(&mut io)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

        // Read the init message body.
        let mut body = vec![0; length];
        io.read_exact(&mut body)?;

        let (parties, rx) = Peerset::from_bytes(&*peerset_buffer, local_peer_id);
        Ok((Self { parties, body }, rx))
    }

    fn to_bytes(self) -> io::Result<Vec<u8>> {
        let b = vec![];
        let mut io = BufWriter::new(b);

        let peerset_bytes = self.parties.to_bytes();

        // Write the peerset payload size.
        {
            let mut buffer = unsigned_varint::encode::usize_buffer();
            io.write_all(unsigned_varint::encode::usize(
                peerset_bytes.len(),
                &mut buffer,
            ))?;
        }

        io.write_all(&*peerset_bytes)?;

        // Write the body payload length.
        {
            let mut buffer = unsigned_varint::encode::usize_buffer();
            io.write_all(unsigned_varint::encode::usize(self.body.len(), &mut buffer))?;
        }

        // Write the init message.
        io.write_all(&self.body)?;

        Ok(io.buffer().to_vec())
    }
}

#[cfg(test)]
mod tests {
    use crate::negotiation::StartMsg;
    use crate::peerset::Peerset;
    use libp2p::PeerId;
    use std::str::FromStr;

    #[test]
    fn start_msg_encoding() {
        let peer_ids = vec![
            PeerId::from_str("12D3KooWMQmcJA5raTtuxqAguM5CiXRhEDumLNmZQ7PmKZizjFBX").unwrap(),
            PeerId::from_str("12D3KooWS4jk2BXKgyqygNEZScHSzntTKQCdHYiHRrZXiNE9mNHi").unwrap(),
            PeerId::from_str("12D3KooWHYG3YsVs9hTwbgPKVrTrPQBKc8FnDhV6bsJ4W37eds8p").unwrap(),
        ];
        let local_peer_id = peer_ids[0];
        let (mut peerset, _) = Peerset::new(peer_ids.into_iter(), local_peer_id);
        peerset.parties_indexes = vec![1, 2];
        let start_msg = StartMsg {
            parties: peerset.clone(),
            body: vec![1, 2, 3],
        };
        let encoded = StartMsg {
            parties: peerset,
            body: vec![1, 2, 3],
        }
        .to_bytes()
        .unwrap();
        let (decoded, _) = StartMsg::from_bytes(&*encoded, local_peer_id).unwrap();

        println!(
            "original: {:?}, {:?}",
            start_msg.parties.parties_indexes,
            start_msg.parties.clone().remotes_iter().collect::<Vec<_>>()
        );
        println!(
            "decoded: {:?}, {:?}",
            decoded.parties.parties_indexes,
            decoded.parties.clone().remotes_iter().collect::<Vec<_>>()
        );

        assert_eq!(
            start_msg.parties.parties_indexes,
            decoded.parties.parties_indexes
        );
    }
}
