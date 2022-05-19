use crate::coordination::{LocalRpcMsg, Phase1Channel};
use crate::network_proxy::ReceiverProxy;
use crate::peerset::Peerset;
use crate::ComputeAgentAsync;
use async_std::stream;
use async_std::stream::Interval;
use futures::channel::{mpsc, oneshot};
use futures::Stream;
use futures_util::stream::{iter, FuturesOrdered};
use futures_util::FutureExt;
use libp2p::PeerId;
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
}

impl NegotiationChan {
    pub fn new(
        room_id: RoomId,
        room_rx: mpsc::Receiver<broadcast::IncomingMessage>,
        n: u16,
        args: Vec<u8>,
        service: NetworkService,
        agent: Box<dyn ComputeAgentAsync>,
    ) -> Self {
        let local_peer_id = service.local_peer_id();
        Self {
            rx: Some(room_rx),
            timeout: stream::interval(Duration::from_secs(60)),
            agent: Some(agent),
            state: Some(NegotiationState {
                id: room_id,
                n,
                args,
                service,
                peers: iter::once(local_peer_id).collect(),
                responses: None,
                pending_futures: Default::default(),
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
                        let parties =
                            Peerset::new(peers.clone().into_iter(), service.local_peer_id());
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
                                        protocol_id: 0,
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
                            room_receiver,
                            receiver_proxy,
                            parties,
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
            responses.insert(rx);
        }

        // It took too long for peerset to be assembled  - reset to Phase 1.
        if let Poll::Ready(Some(())) = Stream::poll_next(Pin::new(&mut self.timeout), cx) {
            let (ch, tx) = Phase1Channel::new(id.clone(), self.rx.take().unwrap(), service.clone());
            return Poll::Ready(NegotiationMsg::Abort(id.clone(), ch, tx));
        }

        self.state.insert(NegotiationState {
            id,
            n,
            args,
            service,
            peers,
            responses,
            pending_futures,
        });

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
        args: Vec<u8>,
    },
    Abort(RoomId, Phase1Channel, oneshot::Sender<LocalRpcMsg>),
}

pub(crate) struct StartMsg {
    pub parties: Peerset,
    pub body: Vec<u8>,
}

impl StartMsg {
    pub(crate) fn from_bytes(b: Vec<u8>, local_peer_id: PeerId) -> io::Result<Self> {
        let mut peers = vec![];
        let mut io = BufReader::new(b);

        // Read the length.
        let peerset_size = unsigned_varint::io::read_usize(&mut io)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

        for _ in 0..peerset_size {
            let mut buffer = [0u8; 38];
            io.read_exact(&mut buffer)?;
            peers.push(PeerId::from_bytes(&buffer).unwrap())
        }

        // Read the length.
        let length = unsigned_varint::io::read_usize(&mut io)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

        // Read the init message body.
        let mut body = vec![0; length];
        io.read_exact(&mut body)?;

        Ok(Self {
            parties: Peerset::new(peers.into_iter(), local_peer_id),
            body,
        })
    }

    fn to_bytes(self) -> io::Result<Vec<u8>> {
        let mut b = vec![];
        let mut io = BufWriter::new(b);

        // Read the peerset size.
        {
            let mut buffer = unsigned_varint::encode::usize_buffer();
            io.write_all(unsigned_varint::encode::usize(
                self.parties.size(),
                &mut buffer,
            ))?;
        }

        for peer_id in self.parties {
            io.write_all(&*peer_id.to_bytes())?;
        }

        // Write the length.
        {
            let mut buffer = unsigned_varint::encode::usize_buffer();
            io.write_all(unsigned_varint::encode::usize(self.body.len(), &mut buffer))?;
        }

        // Write the init message.
        io.write_all(&self.body)?;

        Ok(io.buffer().to_vec())
    }
}
