use futures::{AsyncRead, AsyncWrite};
use futures_util::{AsyncReadExt, AsyncWriteExt};
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
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;
use std::error::Error;
use std::io::ErrorKind;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use std::{io, iter};

/// Implementation of `NetworkBehaviour` that coordinates the nodes on the network.
pub struct CoordinationBehaviour {
    rooms: HashMap<Cow<'static, str>, (SetId, RequestResponse<CoordCodec>)>,
    peerset: Peerset,
}

impl CoordinationBehaviour {
    pub fn new(rooms: impl Iterator<Item = crate::RoomConfig>, peerset: Peerset) -> Self {
        let mut cfg = RequestResponseConfig::default();
        cfg.set_connection_keep_alive(Duration::from_secs(20));

        let rooms: HashMap<_, _> = rooms
            .map(|rc| {
                (
                    Cow::Owned(rc.name.to_owned()),
                    (
                        SetId::from(rc.set),
                        RequestResponse::new(
                            CoordCodec,
                            iter::once((
                                format!("/rooms/0.1.0/{}", rc.name).as_bytes().to_vec(),
                                ProtocolSupport::Full,
                            )),
                            cfg.clone(),
                        ),
                    ),
                )
            })
            .collect();

        CoordinationBehaviour { rooms, peerset }
    }
}

impl NetworkBehaviour for CoordinationBehaviour {
    type ProtocolsHandler = MultiHandler<
        Cow<'static, str>,
        <RequestResponse<CoordCodec> as NetworkBehaviour>::ProtocolsHandler,
    >;
    type OutEvent = ();

    fn new_handler(&mut self) -> Self::ProtocolsHandler {
        let iter = self
            .rooms
            .iter_mut()
            .map(|(room, r)| (room, NetworkBehaviour::new_handler(r)));

        MultiHandler::try_from_iter(iter).unwrap()
    }

    fn addresses_of_peer(&mut self, _: &PeerId) -> Vec<Multiaddr> {
        vec![]
    }

    fn inject_connected(&mut self, peer_id: &PeerId) {
        for (_, p) in self.rooms.values_mut() {
            p.inject_connected(peer_id);
        }
    }

    fn inject_disconnected(&mut self, peer_id: &PeerId) {
        for (_, p) in self.rooms.values_mut() {
            p.inject_disconnected(peer_id);
        }
    }

    fn inject_connection_established(
        &mut self,
        peer_id: &PeerId,
        conn: &ConnectionId,
        endpoint: &ConnectedPoint,
        failed_addresses: Option<&Vec<Multiaddr>>,
    ) {
        for (set_id, p) in self.rooms.values_mut() {
            p.inject_connection_established(peer_id, conn, endpoint, failed_addresses);
            self.peerset.incoming_connection(set_id.clone(), peer_id);
        }
    }

    fn inject_connection_closed(
        &mut self,
        peer_id: &PeerId,
        conn: &ConnectionId,
        endpoint: &ConnectedPoint,
        _: <Self::ProtocolsHandler as IntoProtocolsHandler>::Handler,
    ) {
        for (set_id, p) in self.rooms.values_mut() {
            p.inject_connection_closed(peer_id, conn, endpoint, p.new_handler());
            self.peerset.closed_connection(set_id.clone(), peer_id);
        }
    }

    fn inject_event(
        &mut self,
        peer_id: PeerId,
        connection: ConnectionId,
        (room, event): <Self::ProtocolsHandler as ProtocolsHandler>::OutEvent,
    ) {
        if let Some((_, p)) = self.rooms.get_mut(room) {
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
        for (_, p) in self.rooms.values_mut() {
            p.inject_dial_failure(peer_id, p.new_handler(), error)
        }
    }

    fn inject_new_listener(&mut self, id: ListenerId) {
        for (_, p) in self.rooms.values_mut() {
            p.inject_new_listener(id)
        }
    }

    fn inject_new_listen_addr(&mut self, id: ListenerId, addr: &Multiaddr) {
        for (_, p) in self.rooms.values_mut() {
            p.inject_new_listen_addr(id, addr)
        }
    }

    fn inject_expired_listen_addr(&mut self, id: ListenerId, addr: &Multiaddr) {
        for (_, p) in self.rooms.values_mut() {
            p.inject_expired_listen_addr(id, addr)
        }
    }

    fn inject_listener_error(&mut self, id: ListenerId, err: &(dyn std::error::Error + 'static)) {
        for (_, p) in self.rooms.values_mut() {
            p.inject_listener_error(id, err)
        }
    }

    fn inject_listener_closed(&mut self, id: ListenerId, reason: Result<(), &io::Error>) {
        for (_, p) in self.rooms.values_mut() {
            p.inject_listener_closed(id, reason)
        }
    }

    fn inject_new_external_addr(&mut self, addr: &Multiaddr) {
        for (_, p) in self.rooms.values_mut() {
            p.inject_new_external_addr(addr)
        }
    }

    fn inject_expired_external_addr(&mut self, addr: &Multiaddr) {
        for (_, p) in self.rooms.values_mut() {
            p.inject_expired_external_addr(addr)
        }
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
        _: &mut impl PollParameters,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
enum ElectionMessage {
    Propose,
    Aye,
    Nae,
}

#[async_trait::async_trait]
impl RequestResponseCodec for CoordCodec {
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
        let length = unsigned_varint::aio::read_usize(&mut io)
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        if length > 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Request size exceeds limit: {} > {}", length, 1024),
            ));
        }

        // Read the payload.
        let mut buffer = vec![0; length];
        io.read_exact(&mut buffer).await?;

        serde_ipld_dagcbor::from_slice(&*buffer)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
    }

    async fn read_response<T>(
        &mut self,
        _: &Self::Protocol,
        mut io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        // Read the length.
        let length = unsigned_varint::aio::read_usize(&mut io)
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        if length > 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Response size exceeds limit: {} > {}", length, 1024),
            ));
        }

        // Read the payload.
        let mut buffer = vec![0; length];
        io.read_exact(&mut buffer).await?;

        serde_ipld_dagcbor::from_slice(&*buffer)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
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
        // Write the length.
        {
            let mut buffer = unsigned_varint::encode::usize_buffer();
            io.write_all(unsigned_varint::encode::usize(
                req.payload.len(),
                &mut buffer,
            ))
            .await?;
        }

        let bytes = serde_ipld_dagcbor::to_vec(&req)
            .map_err(|e| io::Error::new(ErrorKind::InvalidInput, e))?;

        // Write the payload.
        io.write_all(&bytes).await?;
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
        // Write the length.
        {
            let mut buffer = unsigned_varint::encode::usize_buffer();
            io.write_all(unsigned_varint::encode::usize(
                req.payload.len(),
                &mut buffer,
            ))
            .await?;
        }

        let bytes = serde_ipld_dagcbor::to_vec(&res)
            .map_err(|e| io::Error::new(ErrorKind::InvalidInput, e))?;

        // Write the payload.
        io.write_all(&bytes).await?;
        io.close().await?;

        Ok(())
    }
}
