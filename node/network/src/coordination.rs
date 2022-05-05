use futures::AsyncRead;
use libp2p::core::connection::{ConnectionId, ListenerId};
use libp2p::core::ConnectedPoint;
use libp2p::request_response::handler::{RequestResponseHandler, ResponseProtocol};
use libp2p::request_response::{
    ProtocolSupport, RequestResponse, RequestResponseCodec, RequestResponseConfig,
};
use libp2p::swarm::protocols_handler::multi::MultiHandler;
use libp2p::swarm::NetworkBehaviourAction::CloseConnection;
use libp2p::swarm::{
    DialError, IntoProtocolsHandler, NetworkBehaviour, NetworkBehaviourAction, PollParameters,
    ProtocolsHandler,
};
use libp2p::{Multiaddr, PeerId};
use log::error;
use mpc_peerset::{Peerset, PeersetHandle, SetId};
use std::error::Error;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use std::{io, iter};

/// Implementation of `NetworkBehaviour` that coordinates the nodes on the network.
pub struct CoordinationBehaviour {
    sets: Vec<RequestResponse<CoordCodec>>,
    peerset: Peerset,
}

impl CoordinationBehaviour {
    pub fn new(config: crate::NetworkConfig) -> (Self, PeersetHandle) {
        let mut cfg = RequestResponseConfig::default();
        cfg.set_connection_keep_alive(Duration::from_secs(20));

        let election = RequestResponse::new(
            CoordCodec,
            iter::once(("/election/0.1.0".as_bytes().to_vec(), ProtocolSupport::Full)),
            cfg,
        );

        (CoordinationBehaviour { election, peerset }, peerset_handle)
    }

    fn sets_iter_mut(&mut self) -> impl Iterator<Item = (SetId, RequestResponse<CoordCodec>)> {
        self.sets
            .iter_mut()
            .enumerate()
            .map(|(i, p)| (SetId::from(i), p))
    }
}

impl NetworkBehaviour for CoordinationBehaviour {
    type ProtocolsHandler =
        MultiHandler<usize, <RequestResponse<CoordCodec> as NetworkBehaviour>::ProtocolsHandler>;
    type OutEvent = ();

    fn new_handler(&mut self) -> Self::ProtocolsHandler {
        let iter = self
            .sets
            .iter_mut()
            .enumerate()
            .map(|(i, r)| (i, NetworkBehaviour::new_handler(r)));

        MultiHandler::try_from_iter(iter).unwrap()
    }

    fn addresses_of_peer(&mut self, _: &PeerId) -> Vec<Multiaddr> {
        vec![]
    }

    fn inject_connected(&mut self, peer_id: &PeerId) {
        for p in self.sets.iter_mut() {
            p.inject_connected(peer_id);
        }
    }

    fn inject_disconnected(&mut self, peer_id: &PeerId) {
        for p in self.sets.iter_mut() {
            p.inject_disconnected(peer_id);
        }
        // todo: forget peer?
    }

    fn inject_connection_established(
        &mut self,
        peer_id: &PeerId,
        conn: &ConnectionId,
        endpoint: &ConnectedPoint,
        failed_addresses: Option<&Vec<Multiaddr>>,
    ) {
        for (set_id, p) in self.sets_iter_mut() {
            p.inject_connection_established(peer_id, conn, endpoint, failed_addresses);
            self.peerset.incoming_connection(set_id, peer_id);
        }
    }

    fn inject_connection_closed(
        &mut self,
        peer_id: &PeerId,
        conn: &ConnectionId,
        endpoint: &ConnectedPoint,
        handler: <Self::ProtocolsHandler as IntoProtocolsHandler>::Handler,
    ) {
        for (set_id, p) in self.sets_iter_mut() {
            p.inject_connection_closed(peer_id, conn, endpoint, p.new_handler());
            self.peerset.closed_connection(set_id, peer_id);
        }
    }

    fn inject_event(
        &mut self,
        peer_id: PeerId,
        connection: ConnectionId,
        (i, event): <Self::ProtocolsHandler as ProtocolsHandler>::OutEvent,
    ) {
        if let Some(p) = self.sets.get(i) {
            return p.inject_event(peer_id, connection, event);
        }

        log::warn!(target: "sub-libp2p",
			"inject_node_event: no request-response instance registered for set {i}")
    }

    fn inject_dial_failure(
        &mut self,
        peer_id: Option<PeerId>,
        _: Self::ProtocolsHandler,
        error: &libp2p::swarm::DialError,
    ) {
        for p in self.sets.iter_mut() {
            p.inject_dial_failure(peer_id, p.new_handler(), error)
        }
    }

    fn inject_new_listener(&mut self, id: ListenerId) {
        for p in self.sets.iter_mut() {
            p.inject_new_listener(id)
        }
    }

    fn inject_new_listen_addr(&mut self, id: ListenerId, addr: &Multiaddr) {
        for p in self.sets.iter_mut() {
            p.inject_new_listen_addr(id, addr)
        }
    }

    fn inject_expired_listen_addr(&mut self, id: ListenerId, addr: &Multiaddr) {
        for p in self.sets.iter_mut() {
            p.inject_expired_listen_addr(id, addr)
        }
    }

    fn inject_listener_error(&mut self, id: ListenerId, err: &(dyn std::error::Error + 'static)) {
        for p in self.sets.iter_mut() {
            p.inject_listener_error(id, err)
        }
    }

    fn inject_listener_closed(&mut self, id: ListenerId, reason: Result<(), &io::Error>) {
        for p in self.sets.iter_mut() {
            p.inject_listener_closed(id, reason)
        }
    }

    fn inject_new_external_addr(&mut self, addr: &Multiaddr) {
        for p in self.sets.iter_mut() {
            p.inject_new_external_addr(addr)
        }
    }

    fn inject_expired_external_addr(&mut self, addr: &Multiaddr) {
        for p in self.sets.iter_mut() {
            p.inject_expired_external_addr(addr)
        }
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
        params: &mut impl PollParameters,
    ) -> Poll<NetworkBehaviourAction<Self::OutEvent, Self::ProtocolsHandler>> {
        loop {
            match futures::Stream::poll_next(Pin::new(&mut self.peerset), cx) {
                Poll::Ready(Some(mpc_peerset::Message::Connect { addr, .. })) => {
                    return Poll::Ready(NetworkBehaviourAction::DialAddress {
                        address: addr,
                        handler: self.broadcast.new_handler(),
                    });
                }
                Poll::Ready(Some(mpc_peerset::Message::Drop { peer_id, .. })) => {
                    return Poll::Ready(NetworkBehaviourAction::CloseConnection {
                        peer_id,
                        connection: CloseConnection::All,
                    });
                }
                // Poll::Ready(Some(Message::GatherSet {
                //     session_id,
                //     protocol_id,
                //     target_size,
                //     on_ready,
                // })) => match self.sessions.entry(session_id) {
                //     Entry::Occupied(_) => {
                //         error!("session with {session_id} already been registered");
                //         drop(on_ready);
                //     }
                //     Entry::Vacant(e) => e.insert(SessionState {
                //         protocol_id: Default::default(),
                //         parties: Default::default(),
                //     }),
                // },
                Poll::Ready(None) => {
                    error!(target: "sub-libp2p", "Peerset receiver stream has returned None");
                    break;
                }
                Poll::Pending => break,
            }
        }

        Poll::Pending
    }
}

struct CoordCodec;

enum ElectionMessage {
    Propose,
    Aye,
    Nae,
}

#[async_trait::async_trait]
impl RequestResponseCodec for CoordCodec {
    type Protocol = SetId;
    type Request = ElectionMessage;
    type Response = ElectionMessage;

    async fn read_request<T>(
        &mut self,
        _: &Self::Protocol,
        mut io: &mut T,
    ) -> io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        Ok()
    }

    async fn read_response<T>(
        &mut self,
        _: &Self::Protocol,
        mut io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        // Note that this function returns a `Result<Result<...>>`. Returning an `Err` is
        // considered as a protocol error and will result in the entire connection being closed.
        // Returning `Ok(Err(_))` signifies that a response has successfully been fetched, and
        // that this response is an error.

        // Read the length.
        let length = match unsigned_varint::aio::read_usize(&mut io).await {
            Ok(l) => l,
            Err(unsigned_varint::io::ReadError::Io(err))
                if matches!(err.kind(), io::ErrorKind::UnexpectedEof) =>
            {
                return Ok(Err(()));
            }
            Err(err) => return Err(io::Error::new(io::ErrorKind::InvalidInput, err)),
        };

        if length > usize::try_from(self.max_response_size).unwrap_or(usize::MAX) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Response size exceeds limit: {} > {}",
                    length, self.max_response_size
                ),
            ));
        }

        // Read the payload.
        let mut buffer = vec![0; length];
        io.read_exact(&mut buffer).await?;
        Ok(Ok(buffer))
    }

    async fn write_request<T>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        // Write broadcast marker
        {
            let mut buffer = unsigned_varint::encode::u8_buffer();
            io.write_all(unsigned_varint::encode::u8(
                req.is_broadcast as u8,
                &mut buffer,
            ))
            .await?;
        }

        // Write session_id
        {
            let mut buffer = unsigned_varint::encode::u64_buffer();
            io.write_all(unsigned_varint::encode::u64(
                req.context.session_id,
                &mut buffer,
            ))
            .await?;
        }

        // Write round_index
        {
            let mut buffer = unsigned_varint::encode::u16_buffer();
            io.write_all(unsigned_varint::encode::u16(
                req.context.round_index,
                &mut buffer,
            ))
            .await?;
        }

        // Write the length.
        {
            let mut buffer = unsigned_varint::encode::usize_buffer();
            io.write_all(unsigned_varint::encode::usize(
                req.payload.len(),
                &mut buffer,
            ))
            .await?;
        }

        // Write the payload.
        io.write_all(&req.payload).await?;

        io.close().await?;
        Ok(())
    }

    async fn write_response<T>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
        res: Self::Response,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        // If `res` is an `Err`, we jump to closing the substream without writing anything on it.
        if let Ok(res) = res {
            // TODO: check the length?
            // Write the length.
            {
                let mut buffer = unsigned_varint::encode::usize_buffer();
                io.write_all(unsigned_varint::encode::usize(res.len(), &mut buffer))
                    .await?;
            }

            // Write the payload.
            io.write_all(&res).await?;
        }

        io.close().await?;
        Ok(())
    }
}
