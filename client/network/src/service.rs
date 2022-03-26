use std::borrow::Borrow;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;
use async_std::channel::{unbounded, Receiver, Sender};
use async_std::{stream, task};
use async_std::stream::StreamExt;
use futures::future::Select;
use futures::select;
use libp2p::identity::Keypair;
use libp2p::{mplex, Multiaddr, noise, PeerId, Swarm, Transport};
use libp2p::core::muxing::StreamMuxerBox;
use libp2p::core::transport::{Boxed, upgrade};
use libp2p::noise::NoiseConfig;
use libp2p::swarm::SwarmEvent;
use libp2p::tcp::TcpConfig;
use log::{debug, error};
use parking_lot::Mutex;
use round_based::Msg;
use serde::de::DeserializeOwned;
use serde::Serialize;
use mpc_peerset::SetId;
use crate::{Behaviour, BehaviourOut, config, P2PConfig, WireRequest};
use crate::mpc_protocol::MPCProtocol;


#[typetag::serde(tag = "type")]
pub trait WireMessage {}

/// Events emitted by this Service.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum NetworkEvent {
    BroadcastMessage(),
}


/// Messages into the service to handle.
#[derive(Debug)]
pub enum NetworkMessage {
    Broadcast(Vec<u16>),
    SendDirect(PeerId, Vec<u16>)
}

/// The Libp2pService listens to events from the Libp2p swarm.
pub struct NetworkWorker
{
    /// The *actual* network.
    swarm: Swarm<Behaviour>,

    peerset: mpc_peerset::Peerset,
    peerset_handle: mpc_peerset::PeersetHandle,

    from_service: Receiver<NetworkMessage>,
    to_service: Sender<NetworkEvent>,
}

pub struct NetworkService
{
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
    from_worker: Receiver<NetworkEvent>,

    peerset: &'static mpc_peerset::Peerset,
}

const MASTER_PEERSET: SetId = SetId(0usize);

impl NetworkWorker
{
    pub fn new(
        params: config::Params,
    ) -> (NetworkWorker, NetworkService) {
        let peer_id = PeerId::from(keypair.public());
        let (mut peerset, peerset_handle) = mpc_peerset::Peerset::from_config(
            mpc_peerset::PeersetConfig{
                sets: vec![config.default_peers_set]
        });

        let transport = {
            let dh_keys = noise::Keypair::<noise::X25519Spec>::new()
                .into_authentic(&params.network_config.keypair)
                .expect("Noise key generation failed");

            TcpConfig::new()
                .upgrade(upgrade::Version::V1)
                .authenticate(NoiseConfig::xx(dh_keys).into_authenticated())
                .multiplex(mplex::MplexConfig::new())
                .boxed()
        };

        let mut swarm = Swarm::new(
            transport,
            Behaviour::new(params.broadcast_protocols, &mut peerset),
            peer_id,
        );

        for peer in peerset.reserved_peers(MASTER_PEERSET) {
            swarm.dial(peer)?;
            println!("Dialed peer {}", peer.to_base58());
        }

        Swarm::listen_on(&mut swarm, config.host).unwrap();

        let (network_sender_in, network_receiver_in) = unbounded();
        let (network_sender_out, network_receiver_out) = unbounded();

        let worker = NetworkWorker {
            swarm,
            peerset,
            peerset_handle: peerset_handle.clone(),
            from_service: network_receiver_in,
            to_service: network_sender_out,
        };

        let service = NetworkService {
            local_peer_id: peer_id,
            num_connected: Arc::new(Default::default()),
            external_addresses: Arc::new(Default::default()),
            to_worker: network_sender_in,
            from_worker: network_receiver_out,
            peerset: &worker.peerset,
            peerset_handle,
        };

        (worker, service)
    }

    /// Starts the libp2p service networking stack.
    pub async fn run(mut self) {
        let mut swarm_stream = self.swarm.fuse();
        let mut network_stream = self.from_service.fuse();
        let mut interval = stream::interval(Duration::from_secs(15)).fuse();

        loop {
            select! {
                swarm_event = swarm_stream.next() => match swarm_event {
                    // outbound events
                    Some(event) => match event {
                        SwarmEvent::Behaviour(MultiPartyBehaviourEvent::<M>::BroadcastMessage{channel, peer, request: message }) => {
                            emit_event(&self.network_sender_out,
                                       NetworkEvent::BroadcastMessage(message)).await;
                        }
                    }
                },
                rpc_message = network_stream.next() => match rpc_message {
                    // Inbound messages
                    Some(request) => match request {
                        NetworkMessage::Broadcast(message) => {
                            self.broadcast_message(message)
                        }
                    }
                    None => { break; }
                },
                interval_event = interval.next() => if interval_event.is_some() {
                    // Print peer count on an interval.
                    debug!("Peers connected: {}", swarm_stream.get_mut().behaviour_mut().peers().len());
                }
            }
        }
    }
}

impl NetworkService {
    fn broadcast_message(&self, payload: Vec<u16>) {
        self.to_worker.send(NetworkMessage::Broadcast(payload));
    }

    fn send_message(&self, peer: PeerId, payload: Vec<u16>) {
        self.to_worker.send(NetworkMessage::SendDirect(peer, payload));
    }
}
