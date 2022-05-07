use crate::broadcast::{Broadcast, IncomingMessage, ProtocolConfig};
use crate::RoomConfig;
use futures::channel::mpsc;
use futures::{AsyncRead, AsyncWrite};
use futures_util::{AsyncReadExt, AsyncWriteExt, StreamExt};
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
    rooms: Vec<(RoomConfig, mpsc::Receiver<IncomingMessage>)>,
    broadcast: Broadcast,
    peerset: Peerset,
}

impl CoordinationBehaviour {
    pub fn new(room_configs: impl Iterator<Item = crate::RoomConfig>, peerset: Peerset) -> Self {
        let mut broadcast_protocols = vec![];

        let mut rooms = vec![];

        for rc in room_configs {
            let protocol_id = Cow::Owned(format!("/room/0.1.0/{}", rc.name);
            let (proto_cfg, rx) = ProtocolConfig::new_with_receiver(
                protocol_id,
                rc.target_size,
            );

            broadcast_protocols.push(proto_cfg);
            rooms.push((rc, rx));
        }

        let broadcast = Broadcast::new(broadcast_protocols, peerset.get_handle()).unwrap();

        CoordinationBehaviour {
            rooms,
            broadcast,
            peerset,
        }
    }
}

impl NetworkBehaviour for CoordinationBehaviour {
    type ProtocolsHandler = <Broadcast as NetworkBehaviour>::ProtocolsHandler;
    type OutEvent = ();

    fn new_handler(&mut self) -> Self::ProtocolsHandler {
        self.broadcast.new_handler()
    }

    fn addresses_of_peer(&mut self, _: &PeerId) -> Vec<Multiaddr> {
        vec![]
    }

    fn inject_connected(&mut self, peer_id: &PeerId) {
        self.broadcast.inject_connected(peer_id);
    }

    fn inject_disconnected(&mut self, peer_id: &PeerId) {
        self.broadcast.inject_disconnected(peer_id);
    }

    fn inject_connection_established(
        &mut self,
        peer_id: &PeerId,
        conn: &ConnectionId,
        endpoint: &ConnectedPoint,
        failed_addresses: Option<&Vec<Multiaddr>>,
    ) {
        self.broadcast
            .inject_connection_established(peer_id, conn, endpoint, failed_addresses);

        for room in self.rooms.iter() {
            self.peerset.incoming_connection(room.set.into(), peer_id);
        }
    }

    fn inject_connection_closed(
        &mut self,
        peer_id: &PeerId,
        conn: &ConnectionId,
        endpoint: &ConnectedPoint,
        _: <Self::ProtocolsHandler as IntoProtocolsHandler>::Handler,
    ) {
        self.broadcast.inject_connection_closed(
            peer_id,
            conn,
            endpoint,
            self.broadcast.new_handler(),
        );

        for room in self.rooms.iter() {
            self.peerset.closed_connection(room.set.into(), peer_id);
        }
    }

    fn inject_event(
        &mut self,
        peer_id: PeerId,
        connection: ConnectionId,
        (room, event): <Self::ProtocolsHandler as ProtocolsHandler>::OutEvent,
    ) {
        self.broadcast
            .inject_event(peer_id, connection, (room, event));
    }

    fn inject_dial_failure(
        &mut self,
        peer_id: Option<PeerId>,
        _: Self::ProtocolsHandler,
        error: &libp2p::swarm::DialError,
    ) {
        self.broadcast
            .inject_dial_failure(peer_id, p.new_handler(), error)
    }

    fn inject_new_listener(&mut self, id: ListenerId) {
        self.broadcast.inject_new_listener(id)
    }

    fn inject_new_listen_addr(&mut self, id: ListenerId, addr: &Multiaddr) {
        self.broadcast.inject_new_listen_addr(id, addr)
    }

    fn inject_expired_listen_addr(&mut self, id: ListenerId, addr: &Multiaddr) {
        self.broadcast.inject_expired_listen_addr(id, addr)
    }

    fn inject_listener_error(&mut self, id: ListenerId, err: &(dyn std::error::Error + 'static)) {
        self.broadcast.inject_listener_error(id, err)
    }

    fn inject_listener_closed(&mut self, id: ListenerId, reason: Result<(), &io::Error>) {
        self.broadcast.inject_listener_closed(id, reason)
    }

    fn inject_new_external_addr(&mut self, addr: &Multiaddr) {
        self.broadcast.inject_new_external_addr(addr)
    }

    fn inject_expired_external_addr(&mut self, addr: &Multiaddr) {
        self.broadcast.inject_expired_external_addr(addr)
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
