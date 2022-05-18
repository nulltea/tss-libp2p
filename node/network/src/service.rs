use crate::broadcast::IfDisconnected;
use crate::error::Error;
use crate::{
    behaviour::{Behaviour, BehaviourOut},
    broadcast, MessageContext, NodeKeyConfig, RoomId,
};
use async_std::channel::{unbounded, Receiver, Sender};
use futures::channel::mpsc;
use futures::select;
use futures_util::stream::StreamExt;
use libp2p::core::transport::upgrade;

use libp2p::noise::NoiseConfig;
use libp2p::swarm::SwarmEvent;
use libp2p::tcp::TcpConfig;
use libp2p::{mplex, noise, PeerId, Swarm, Transport};
use log::{info, warn};
use std::borrow::Cow;

/// Events emitted by this Service.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum NetworkEvent {
    BroadcastMessage(PeerId, Cow<'static, str>),
}

/// Messages into the service to handle.
#[derive(Debug)]
pub enum NetworkMessage {
    RequestResponse {
        room_id: RoomId,
        context: MessageContext,
        message: MessageRouting,
    },
}

#[derive(Debug)]
pub enum MessageRouting {
    Broadcast(
        Vec<u8>,
        Option<mpsc::Sender<Result<(PeerId, Vec<u8>), broadcast::RequestFailure>>>,
    ),
    Multicast(
        Vec<PeerId>,
        Vec<u8>,
        Option<mpsc::Sender<Result<(PeerId, Vec<u8>), broadcast::RequestFailure>>>,
    ),
    SendDirect(
        PeerId,
        Vec<u8>,
        mpsc::Sender<Result<(PeerId, Vec<u8>), broadcast::RequestFailure>>,
    ),
}

/// The Libp2pService listens to events from the Libp2p swarm.
pub struct NetworkWorker {
    swarm: Swarm<Behaviour>,
    from_service: Receiver<NetworkMessage>,
    local_peer_id: PeerId,
}

#[derive(Clone)]
pub struct NetworkService {
    /// Local copy of the `PeerId` of the local node.
    local_peer_id: PeerId,
    /// Channel for sending requests to worker.
    to_worker: Sender<NetworkMessage>,
}

impl NetworkWorker {
    pub fn new(
        node_key: NodeKeyConfig,
        params: crate::Params,
    ) -> Result<(NetworkWorker, NetworkService), Error> {
        let keypair = node_key.into_keypair().map_err(|e| Error::Io(e))?;
        let local_peer_id = PeerId::from(keypair.public());
        info!(
            target: "sub-libp2p",
            "🏷 Local node identity is: {}",
            local_peer_id.to_base58(),
        );

        let transport = {
            let dh_keys = noise::Keypair::<noise::X25519Spec>::new()
                .into_authentic(&keypair)
                .expect("Noise key generation failed");

            TcpConfig::new()
                .upgrade(upgrade::Version::V1)
                .authenticate(NoiseConfig::xx(dh_keys).into_authenticated())
                .multiplex(mplex::MplexConfig::new())
                .boxed()
        };

        let mut broadcast_protocols = vec![];

        for rc in params.rooms.clone() {
            let protocol_id = Cow::Owned(rc.id.as_protocol_id().to_string());
            let proto_cfg = broadcast::ProtocolConfig::new(protocol_id, rc.inbound_queue);

            broadcast_protocols.push(proto_cfg);
        }

        let behaviour = {
            match Behaviour::new(&keypair, broadcast_protocols, params.clone()) {
                Ok(b) => b,
                Err(crate::broadcast::RegisterError::DuplicateProtocol(proto)) => {
                    return Err(Error::DuplicateBroadcastProtocol { protocol: proto });
                }
            }
        };

        let mut swarm = Swarm::new(transport, behaviour, local_peer_id);

        // Listen on the addresses.
        if let Err(err) = swarm.listen_on(params.listen_address) {
            warn!(target: "sub-libp2p", "Can't listen on 'listen_address' because: {:?}", err)
        }

        let (network_sender_in, network_receiver_in) = unbounded();

        let worker = NetworkWorker {
            local_peer_id,
            swarm,
            from_service: network_receiver_in,
        };

        let service = NetworkService {
            local_peer_id,
            to_worker: network_sender_in,
        };

        Ok((worker, service))
    }

    /// Starts the libp2p service networking stack.
    pub async fn run(mut self) {
        // Bootstrap with Kademlia
        if let Err(e) = self.swarm.behaviour_mut().bootstrap() {
            warn!("Failed to bootstrap with Kademlia: {}", e);
        }

        let mut swarm_stream = self.swarm.fuse();
        let mut network_stream = self.from_service.fuse();

        loop {
            select! {
                swarm_event = swarm_stream.next() => match swarm_event {
                    // Outbound events
                    Some(event) => match event {
                        SwarmEvent::Behaviour(BehaviourOut::InboundMessage{peer, protocol}) => {
                            info!("Inbound message from {:?} related to {:?} protocol", peer, protocol);
                        },
                        SwarmEvent::NewListenAddr { address, .. } => info!("Listening on {:?}", address),
                        SwarmEvent::ConnectionEstablished { peer_id: _, .. } => {

                        },
                        SwarmEvent::ConnectionClosed { peer_id: _, .. } => { }
                        _ => continue
                    }
                    None => { break; }
                },
                rpc_message = network_stream.next() => match rpc_message {
                    // Inbound requests
                    Some(request) => {
                        let behaviour = swarm_stream.get_mut().behaviour_mut();

                        match request {
                            NetworkMessage::RequestResponse {
                                room_id,
                                context,
                                message
                            } => {

                                match message {
                                    MessageRouting::Broadcast(payload, response_sender) => {
                                        behaviour.broadcast_message(
                                            behaviour.peers(room_id),
                                            payload,
                                            room_id,
                                            context,
                                            response_sender,
                                            IfDisconnected::ImmediateError,
                                        )
                                    }
                                    MessageRouting::Multicast(peer_ids, payload, response_sender) => {
                                        behaviour.broadcast_message(
                                            peer_ids.into_iter(),
                                            payload,
                                            room_id,
                                            context,
                                            response_sender,
                                            IfDisconnected::ImmediateError,
                                        )
                                    }
                                    MessageRouting::SendDirect(peer_id, payload, response_sender) => {
                                        behaviour.send_message(
                                            &peer_id,
                                            payload,
                                            room_id,
                                            context,
                                            response_sender,
                                            IfDisconnected::ImmediateError,
                                        )
                                    }
                                }
                            }
                        }
                    }
                    None => { break; }
                }
            };
        }
    }
}

impl NetworkService {
    pub async fn broadcast_message(
        &self,
        room_id: &RoomId,
        context: MessageContext,
        payload: Vec<u8>,
        response_sender: Option<mpsc::Sender<Result<(PeerId, Vec<u8>), broadcast::RequestFailure>>>,
    ) {
        self.to_worker
            .send(NetworkMessage::RequestResponse {
                room_id: room_id.clone(),
                context,
                message: MessageRouting::Broadcast(payload, response_sender),
            })
            .await;
    }

    pub async fn multicast_message(
        &self,
        room_id: &RoomId,
        peer_ids: impl Iterator<Item = PeerId>,
        context: MessageContext,
        payload: Vec<u8>,
        response_sender: Option<mpsc::Sender<Result<(PeerId, Vec<u8>), broadcast::RequestFailure>>>,
    ) {
        self.to_worker
            .send(NetworkMessage::RequestResponse {
                room_id: room_id.clone(),
                context,
                message: MessageRouting::Multicast(peer_ids.collect(), payload, response_sender),
            })
            .await;
    }

    pub async fn multicast_message_owned(
        self,
        room_id: RoomId,
        peer_ids: impl Iterator<Item = PeerId>,
        context: MessageContext,
        payload: Vec<u8>,
        response_sender: Option<mpsc::Sender<Result<(PeerId, Vec<u8>), broadcast::RequestFailure>>>,
    ) {
        self.to_worker
            .send(NetworkMessage::RequestResponse {
                room_id,
                context,
                message: MessageRouting::Multicast(peer_ids.collect(), payload, response_sender),
            })
            .await;
    }

    pub async fn send_message(
        &self,
        room_id: &RoomId,
        peer_id: PeerId,
        context: MessageContext,
        payload: Vec<u8>,
        response_sender: mpsc::Sender<Result<(PeerId, Vec<u8>), broadcast::RequestFailure>>,
    ) {
        self.to_worker
            .send(NetworkMessage::RequestResponse {
                room_id: room_id.clone(),
                context,
                message: MessageRouting::SendDirect(peer_id, payload, response_sender),
            })
            .await;
    }

    pub async fn send_message_owned(
        self,
        room_id: RoomId,
        peer_id: PeerId,
        context: MessageContext,
        payload: Vec<u8>,
        response_sender: mpsc::Sender<Result<(PeerId, Vec<u8>), broadcast::RequestFailure>>,
    ) {
        self.to_worker
            .send(NetworkMessage::RequestResponse {
                room_id,
                context,
                message: MessageRouting::SendDirect(peer_id, payload, response_sender),
            })
            .await;
    }

    pub fn local_peer_id(&self) -> PeerId {
        self.local_peer_id.clone()
    }
}
