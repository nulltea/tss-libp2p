use libp2p::core::connection::{ConnectionId, ListenerId};
use libp2p::core::ConnectedPoint;
use libp2p::request_response::handler::{RequestResponseHandler, ResponseProtocol};
use libp2p::request_response::{
    ProtocolSupport, RequestResponse, RequestResponseCodec, RequestResponseConfig,
};
use libp2p::swarm::NetworkBehaviourAction::CloseConnection;
use libp2p::swarm::{
    DialError, IntoProtocolsHandler, NetworkBehaviour, NetworkBehaviourAction, PollParameters,
    ProtocolsHandler,
};
use libp2p::{Multiaddr, PeerId};
use log::error;
use mpc_peerset::{Peerset, PeersetHandle};
use std::error::Error;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use std::{io, iter};

/// Implementation of `NetworkBehaviour` that coordinates the nodes on the network.
pub struct CoordinationBehaviour {
    election: RequestResponse<ElectionCodec>,
    peerset: Peerset,
}

impl CoordinationBehaviour {
    pub fn new(config: crate::NetworkConfig) -> (Self, PeersetHandle) {
        let (peerset, peerset_handle) = {
            let peers = config.bootstrap_peers.into_iter().map(|p| p.peer_id);

            mpc_peerset::Peerset::from_config(
                local_peer_id.clone(),
                mpc_peerset::PeersetConfig::new(peers, 0),
            )
        };

        let mut cfg = RequestResponseConfig::default();
        cfg.set_connection_keep_alive(Duration::from_secs(20));

        let election = RequestResponse::new(
            ElectionCodec,
            iter::once(("/election/0.1.0".as_bytes().to_vec(), ProtocolSupport::Full)),
            cfg,
        );

        (CoordinationBehaviour { election, peerset }, peerset_handle)
    }
}

impl NetworkBehaviour for CoordinationBehaviour {
    type ProtocolsHandler = RequestResponseHandler<ElectionCodec>;
    type OutEvent = ();

    fn new_handler(&mut self) -> Self::ProtocolsHandler {
        NetworkBehaviour::new_handler(&mut self.election)
    }

    fn addresses_of_peer(&mut self, _: &PeerId) -> Vec<Multiaddr> {
        Vec::new()
    }

    fn inject_connected(&mut self, peer_id: &PeerId) {
        self.election.inject_connected(peer_id)
    }

    fn inject_disconnected(&mut self, peer_id: &PeerId) {
        self.election
            .inject_connection_established(peer_id, conn, endpoint, handler)
    }

    fn inject_connection_established(
        &mut self,
        peer_id: &PeerId,
        conn: &ConnectionId,
        endpoint: &ConnectedPoint,
        failed_addresses: Option<&Vec<Multiaddr>>,
    ) {
        self.election
            .inject_connection_established(peer_id, conn, endpoint, failed_addresses)
    }

    fn inject_connection_closed(
        &mut self,
        peer_id: &PeerId,
        conn: &ConnectionId,
        endpoint: &ConnectedPoint,
        handler: <Self::ProtocolsHandler as IntoProtocolsHandler>::Handler,
    ) {
        self.election
            .inject_connection_closed(peer_id, conn, endpoint, handler)
    }

    fn inject_event(
        &mut self,
        peer_id: PeerId,
        connection: ConnectionId,
        event: <Self::ProtocolsHandler as ProtocolsHandler>::OutEvent,
    ) {
        self.election.inject_event(peer_id, connection, event)
    }

    fn inject_dial_failure(
        &mut self,
        peer_id: Option<PeerId>,
        handler: Self::ProtocolsHandler,
        error: &libp2p::swarm::DialError,
    ) {
        self.election.inject_dial_failure(peer_id, handler, error)
    }

    fn inject_new_listener(&mut self, id: ListenerId) {
        self.election.inject_new_listener(id)
    }

    fn inject_new_listen_addr(&mut self, id: ListenerId, addr: &Multiaddr) {
        self.election.inject_new_listen_addr(id, addr)
    }

    fn inject_expired_listen_addr(&mut self, id: ListenerId, addr: &Multiaddr) {
        self.election.inject_expired_listen_addr(id, addr)
    }

    fn inject_listener_error(&mut self, id: ListenerId, err: &(dyn std::error::Error + 'static)) {
        self.election.inject_listener_error(id, err)
    }

    fn inject_listener_closed(&mut self, id: ListenerId, reason: Result<(), &io::Error>) {
        self.election.inject_listener_closed(id, reason)
    }

    fn inject_new_external_addr(&mut self, addr: &Multiaddr) {
        self.election.inject_new_external_addr(addr)
    }

    fn inject_expired_external_addr(&mut self, addr: &Multiaddr) {
        self.election.inject_expired_external_addr(addr)
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
        params: &mut impl PollParameters,
    ) -> Poll<NetworkBehaviourAction<Self::OutEvent, Self::ProtocolsHandler>> {
        loop {
            match futures::Stream::poll_next(Pin::new(&mut self.peerset), cx) {
                Poll::Ready(Some(mpc_peerset::Message::Connect(addr))) => {
                    return Poll::Ready(NetworkBehaviourAction::DialAddress {
                        address: addr,
                        handler: self.new_handler(),
                    });
                }
                Poll::Ready(Some(mpc_peerset::Message::Drop(peer_id))) => {
                    return Poll::Ready(NetworkBehaviourAction::CloseConnection {
                        peer_id,
                        connection: CloseConnection::All,
                    });
                }
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

struct ElectionCodec;

enum ElectionMessage {
    Alive,
    Election,
}

#[async_trait::async_trait]
impl RequestResponseCodec for ElectionCodec {
    type Protocol = Vec<u8>;
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
        Ok(WireMessage {
            context: ProtoContext {
                session_id,
                round_index,
            },
            payload: buffer,
            is_broadcast: is_broadcast != 0,
        })
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
