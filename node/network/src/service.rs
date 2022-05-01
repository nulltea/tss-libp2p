use crate::broadcast::{IfDisconnected, ProtoContext, ProtocolConfig};
use crate::error::Error;
use crate::{
    behaviour::{Behaviour, BehaviourOut},
    broadcast, config, NodeKeyConfig,
};
use async_std::channel::{unbounded, Receiver, Sender};
use futures::channel::mpsc;
use futures::select;
use futures_util::stream::StreamExt;
use libp2p::core::transport::upgrade;
use libp2p::identify::{Identify, IdentifyConfig};
use libp2p::noise::NoiseConfig;
use libp2p::swarm::SwarmEvent::Behaviour;
use libp2p::swarm::{AddressScore, SwarmEvent};
use libp2p::tcp::TcpConfig;
use libp2p::{mplex, noise, Multiaddr, PeerId, Swarm, Transport};
use log::{error, info, warn};
use mpc_peerset::MembershipState;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::thread::sleep;
use std::time::Duration;

/// Events emitted by this Service.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum NetworkEvent {
    BroadcastMessage(PeerId, Cow<'static, str>),
}

/// Messages into the service to handle.
#[derive(Debug)]
pub enum NetworkMessage {
    CrossRoundExchange {
        session_id: u64,
        protocol_id: Cow<'static, str>,
        round_index: u16,
        message: MessageRouting,
    },
}

#[derive(Debug)]
pub enum MessageRouting {
    Broadcast(
        Vec<u8>,
        mpsc::Sender<Result<Vec<u8>, broadcast::RequestFailure>>,
    ),
    SendDirect(
        u16,
        Vec<u8>,
        mpsc::Sender<Result<Vec<u8>, broadcast::RequestFailure>>,
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
    /// Peerset manager.
    peerset: mpc_peerset::PeersetHandle,
    /// Channel for sending requests to worker.
    to_worker: Sender<NetworkMessage>,
}

impl NetworkWorker {
    pub fn new(
        node_key: NodeKeyConfig,
        params: config::Params,
    ) -> Result<(NetworkWorker, NetworkService), Error> {
        let keypair = node_key.into_keypair().map_err(|e| Error::Io(e))?;
        let local_peer_id = PeerId::from(keypair.public());
        info!(
            target: "sub-libp2p",
            "🏷 Local node identity is: {}",
            local_peer_id.to_base58(),
        );

        let (peerset, peerset_handle) = {
            let peers = params
                .network_config
                .bootstrap_peers
                .into_iter()
                .map(|p| p.peer_id);

            mpc_peerset::Peerset::from_config(
                local_peer_id.clone(),
                mpc_peerset::PeersetConfig::new_static(peers),
            )
        };

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

        let behaviour = {
            let mut broadcast_protocols = params.broadcast_protocols;
            broadcast_protocols.push(ProtocolConfig {
                name: Default::default(),
                max_request_size: 0,
                max_response_size: 0,
                request_timeout: Default::default(),
                inbound_queue: None,
            });

            match Behaviour::new(&keypair, &params, peerset) {
                Ok(b) => b,
                Err(crate::broadcast::RegisterError::DuplicateProtocol(proto)) => {
                    return Err(Error::DuplicateBroadcastProtocol { protocol: proto });
                }
            };
        };

        let mut swarm = Swarm::new(transport, behaviour, local_peer_id);

        // Listen on the addresses.
        if let Err(err) = swarm.listen_on(params.network_config.listen_address) {
            warn!(target: "sub-libp2p", "Can't listen on {} because: {:?}", addr, err)
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
            peerset: peerset_handle,
        };

        Ok((worker, service))
    }

    /// Starts the libp2p service networking stack.
    pub async fn run(self) {
        let mut swarm_stream = self.swarm.fuse();
        let mut network_stream = self.from_service.fuse();
        //let mut connected_peers = HashSet::new();

        // loop {
        //     // minus ourselves
        //     if connected_peers.len() < self.addresses.len() - 1 {
        //         sleep(Duration::from_secs(1));
        //
        //         let mut just_connected = vec![];
        //         for (peer_id, addr) in self.addresses.iter().filter(|(p, _)| {
        //             p.to_bytes() != self.local_peer_id.to_bytes()
        //                 && !connected_peers.contains(p.clone())
        //         }) {
        //             println!("peer_id {:?} addr {:?}", peer_id, addr);
        //             match swarm_stream.get_ref().behaviour().peer_membership(peer_id) {
        //                 MembershipState::NotMember => {
        //                     warn!("all peers are expected to discovered at this point")
        //                 }
        //                 MembershipState::Connected => {
        //                     just_connected.push(peer_id.clone());
        //                 }
        //                 MembershipState::NotConnected { .. } => {
        //                     if swarm_stream.get_mut().dial_addr(addr.clone()).is_err() {
        //                         error!("Failed dealing peer {}", peer_id.to_base58());
        //                     }
        //                 }
        //             }
        //         }
        //
        //         for connected_peer in just_connected.into_iter() {
        //             connected_peers.insert(connected_peer);
        //         }
        //     }

            select! {
                swarm_event = swarm_stream.next() => match swarm_event {
                    // Outbound events
                    Some(event) => match event {
                        SwarmEvent::Behaviour(BehaviourOut::InboundMessage{peer, protocol}) => {
                            info!("Inbound message from {:?} related to {:?} protocol", peer, protocol);
                        },
                        SwarmEvent::NewListenAddr { address, .. } => info!("Listening on {:?}", address),
                        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                            swarm_stream
                                .get_mut()
                                .behaviour_mut()
                                .mark_peer_as_connected(peer_id);
                        },
                        SwarmEvent::ConnectionClosed { peer_id, .. } => {
                            swarm_stream
                                .get_mut()
                                .behaviour_mut()
                                .mark_peer_as_disconnected(peer_id);
                        }
                        _ => continue
                    }
                    None => { break; }
                },
                rpc_message = network_stream.next() => match rpc_message {
                    // Inbound requests
                    Some(request) => {
                        let behaviour = swarm_stream.get_mut().behaviour_mut();

                        match request {
                            NetworkMessage::CrossRoundExchange {
                                session_id,
                                protocol_id,
                                round_index,
                                message
                            } => {
                                let ctx = ProtoContext {
                                    session_id,
                                    round_index,
                                };

                                match message {
                                    MessageRouting::Broadcast(payload, response_sender) => {
                                        behaviour.broadcast_message(
                                            payload,
                                            &protocol_id,
                                            ctx,
                                            response_sender,
                                            IfDisconnected::ImmediateError,
                                        )
                                    }
                                    MessageRouting::SendDirect(receiver_index, payload, response_sender) => {
                                        if let Some(receiver_peer) = behaviour.peer_at_index(receiver_index as usize)
                                        {
                                            behaviour.send_message(
                                                &receiver_peer,
                                                payload,
                                                &protocol_id,
                                                ctx,
                                                response_sender,
                                                IfDisconnected::ImmediateError,
                                            )
                                        } else {
                                            error!("receiver at index ({receiver_index}) does not exists in the set")
                                        }
                                    }
                                }
                            }

                            // match rx.await {
                            //     Ok(_v) => continue,
                            //     Err(e) => error!("failed to wait for response {}", e),
                            // }
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
        session_id: u64,
        protocol_id: String,
        round_index: u16,
        payload: Vec<u8>,
        response_sender: mpsc::Sender<Result<Vec<u8>, broadcast::RequestFailure>>,
    ) {
        self.to_worker
            .send(NetworkMessage::CrossRoundExchange {
                session_id,
                protocol_id: Cow::Owned(protocol_id),
                round_index,
                message: MessageRouting::Broadcast(payload, response_sender),
            })
            .await;
    }

    pub async fn send_message(
        &self,
        session_id: u64,
        protocol_id: String,
        round_index: u16,
        peer_index: u16,
        payload: Vec<u8>,
        response_sender: mpsc::Sender<Result<Vec<u8>, broadcast::RequestFailure>>,
    ) {
        self.to_worker
            .send(NetworkMessage::CrossRoundExchange {
                session_id,
                protocol_id: Cow::Owned(protocol_id),
                round_index,
                message: MessageRouting::SendDirect(peer_index, payload, response_sender),
            })
            .await;
    }

    pub fn local_peer_id(&self) -> PeerId {
        self.local_peer_id.clone()
    }

    pub async fn local_peer_index(&self) -> u16 {
        self.peerset
            .clone()
            .peer_index(self.local_peer_id())
            .await
            .expect("failed determining local peer index")
    }

    pub async fn get_peers(&self) -> impl ExactSizeIterator<Item = PeerId> {
        self.peerset
            .clone()
            .peer_ids()
            .await
            .expect("failed getting peers")
    }
}
