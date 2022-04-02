use crate::broadcast::IfDisconnected;
use crate::error::Error;
use crate::{config, Behaviour, BehaviourOut};
use async_std::channel::{unbounded, Receiver, Sender};
use futures::channel::oneshot;
use futures::select;
use futures_util::stream::StreamExt;
use libp2p::core::transport::upgrade;
use libp2p::identity::Keypair;
use libp2p::noise::NoiseConfig;
use libp2p::swarm::{AddressScore, SwarmEvent};
use libp2p::tcp::TcpConfig;
use libp2p::{mplex, noise, Multiaddr, PeerId, Swarm, Transport};
use log::{error, info, warn};
use mpc_peerset::{Peerset, SetId};
use parking_lot::Mutex;
use std::borrow::Cow;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

/// Events emitted by this Service.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum NetworkEvent {
    BroadcastMessage(PeerId, Cow<'static, str>),
}

/// Messages into the service to handle.
#[derive(Debug)]
pub enum NetworkMessage {
    Broadcast(Cow<'static, str>, Vec<u8>),
    SendDirect(Cow<'static, str>, PeerId, Vec<u8>),
}

/// The Libp2pService listens to events from the Libp2p swarm.
pub struct NetworkWorker {
    /// The *actual* network.
    swarm: Swarm<Behaviour>,

    peerset_handle: mpc_peerset::PeersetHandle,

    from_service: Receiver<NetworkMessage>,
    events_in: Sender<NetworkEvent>,
    events_out: Receiver<NetworkEvent>,
}

#[derive(Clone)]
pub struct NetworkService {
    /// Local copy of the `PeerId` of the local node.
    local_peer_id: PeerId,
    /// Number of peers we're connected to.
    num_connected: Arc<AtomicUsize>,
    /// The local external addresses.
    external_addresses: Arc<Mutex<Vec<Multiaddr>>>,
    /// Peerset manager (PSM); manages the reputation of nodes and indicates the network which
    /// nodes it should be connected to or not.
    peerset_handle: mpc_peerset::PeersetHandle,

    to_worker: Sender<NetworkMessage>,

    peerset: Arc<mpc_peerset::Peerset>,
}

pub const MASTER_PEERSET: SetId = SetId(0usize);

impl NetworkWorker {
    pub fn new(
        keypair: Keypair,
        params: config::Params,
    ) -> Result<(NetworkWorker, NetworkService), Error> {
        let peer_id = PeerId::from(keypair.public());
        info!(
            target: "sub-libp2p",
            "🏷 Local node identity is: {}",
            peer_id.to_base58(),
        );

        let (peerset, peerset_handle) =
            mpc_peerset::Peerset::from_config(mpc_peerset::PeersetConfig {
                sets: vec![params.network_config.default_peers_set],
            });

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

        let peerset = Arc::new(peerset);

        let behaviour = {
            let result = Behaviour::new(params.broadcast_protocols, peerset.clone());

            match result {
                Ok(b) => b,
                Err(crate::broadcast::RegisterError::DuplicateProtocol(proto)) => {
                    return Err(Error::DuplicateBroadcastProtocol { protocol: proto })
                }
            }
        };

        let mut swarm = Swarm::new(transport, behaviour, peer_id);

        let mut num_connected = 0;
        for peer in peerset.connected_peers(MASTER_PEERSET) {
            if swarm.dial(peer).is_ok() {
                println!("Dialed peer {}", peer.to_base58());
                num_connected += 1;
            } else {
                println!("Failed dealing peer {}", peer.to_base58());
            }
        }

        // Listen on the addresses.
        for addr in &params.network_config.listen_addresses {
            if let Err(err) = swarm.listen_on(addr.clone()) {
                warn!(target: "sub-libp2p", "Can't listen on {} because: {:?}", addr, err)
            }
        }

        // Add external addresses.
        for addr in &params.network_config.public_addresses {
            swarm.add_external_address(addr.clone(), AddressScore::Infinite);
        }

        let (network_sender_in, network_receiver_in) = unbounded();
        let (network_sender_out, network_receiver_out) = unbounded();

        let worker = NetworkWorker {
            swarm,
            peerset_handle: peerset_handle.clone(),
            from_service: network_receiver_in,
            events_in: network_sender_out,
            events_out: network_receiver_out,
        };

        let service = NetworkService {
            local_peer_id: peer_id,
            num_connected: Arc::new(AtomicUsize::new(num_connected)),
            external_addresses: Arc::new(Mutex::new(params.network_config.public_addresses)),
            to_worker: network_sender_in,
            peerset: peerset.clone(),
            peerset_handle,
        };

        Ok((worker, service))
    }

    /// Starts the libp2p service networking stack.
    pub async fn run(self) {
        let mut swarm_stream = self.swarm.fuse();
        let mut network_stream = self.from_service.fuse();

        loop {
            select! {
                swarm_event = swarm_stream.next() => match swarm_event {
                    // outbound events
                    Some(event) => match event {
                        SwarmEvent::Behaviour(BehaviourOut::InboundMessage{peer, protocol}) => {
                            emit_event(&self.events_in,
                                       NetworkEvent::BroadcastMessage(peer, protocol)).await;
                        },
                        _ => continue
                    }
                    None => { break; }
                },
                rpc_message = network_stream.next() => match rpc_message {
                    // Inbound messages
                    Some(request) => {
                        let (tx, rx) = oneshot::channel();

                        match request {
                            NetworkMessage::Broadcast(protocol, message) => {
                                swarm_stream.get_mut().behaviour_mut().broadcast_message(
                                    &protocol,
                                    message,
                                    tx,
                                    IfDisconnected::ImmediateError
                                )
                            }
                            NetworkMessage::SendDirect(protocol, peer, message) => {
                                swarm_stream.get_mut().behaviour_mut().message_broadcast.send_message(
                                    &peer,
                                    &protocol,
                                    message,
                                    tx,
                                    IfDisconnected::ImmediateError
                                )
                            }
                        }

                        match rx.await {
                            Ok(_v) => continue,
                            Err(_) => error!("failed to wait for response"),
                        }
                    }
                    None => { break; }
                },
            };
        }
    }
}

impl NetworkService {
    pub fn broadcast_message(&self, protocol: &str, payload: Vec<u8>) {
        self.to_worker.send(NetworkMessage::Broadcast(
            Cow::Owned(protocol.to_string()),
            payload,
        ));
    }

    pub fn send_message(&self, protocol: &str, peer: PeerId, payload: Vec<u8>) {
        self.to_worker.send(NetworkMessage::SendDirect(
            Cow::Owned(protocol.to_string()),
            peer,
            payload,
        ));
    }

    pub fn get_peerset(&self) -> &Peerset {
        &self.peerset
    }

    pub fn local_peer_id(&self) -> PeerId {
        self.local_peer_id.clone()
    }

    pub fn local_peer_index(&self) -> usize {
        self.peerset
            .index_of(MASTER_PEERSET, self.local_peer_id)
            .unwrap() // TODO: determine peerset dynamically.
    }
}

async fn emit_event(sender: &Sender<NetworkEvent>, event: NetworkEvent) {
    if sender.send(event).await.is_err() {
        error!("Failed to emit event: Network channel receiver has been dropped");
    }
}
