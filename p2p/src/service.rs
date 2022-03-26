use std::time::Duration;
use async_std::channel::{Receiver, Sender, unbounded};
use async_std::stream;
use async_std::stream::StreamExt;
use futures::future::Select;
use futures::select;
use libp2p::identity::Keypair;
use libp2p::{mplex, noise, PeerId, Swarm, Transport};
use libp2p::core::muxing::StreamMuxerBox;
use libp2p::core::transport::{Boxed, upgrade};
use libp2p::noise::NoiseConfig;
use libp2p::swarm::SwarmEvent;
use libp2p::tcp::TcpConfig;
use log::debug;
use crate::{MultiPartyBehaviour, P2PConfig, WireRequest};

/// Events emitted by this Service.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum NetworkEvent {

}


/// Messages into the service to handle.
#[derive(Debug)]
pub enum NetworkMessage {

}

/// The Libp2pService listens to events from the Libp2p swarm.
pub struct P2PService {
    swarm: Swarm<MultiPartyBehaviour>,

    network_receiver_in: Receiver<NetworkMessage>,
    network_sender_in: Sender<NetworkMessage>,
    network_receiver_out: Receiver<NetworkEvent>,
    network_sender_out: Sender<NetworkEvent>,

    network_name: String,
}

impl P2PService {
    pub fn new(
        keypair: Keypair,
        config: P2PConfig,
        network_name: &str,
    ) -> Self {
        let peer_id = PeerId::from(keypair.public());

        let transport = build_transport(keypair.clone());

        let mut swarm = Swarm::new(
            transport,
            MultiPartyBehaviour::new(&keypair, config.parties.clone(), network_name),
            peer_id,
        );

        Swarm::listen_on(&mut swarm, config.host).unwrap();

        // Subscribe to gossipsub topics with the network name suffix
        for topic in PUBSUB_TOPICS.iter() {
            let t = Topic::new(format!("{}/{}", topic, network_name));
            swarm.behaviour_mut().subscribe(&t).unwrap();
        }

        let (network_sender_in, network_receiver_in) = unbounded();
        let (network_sender_out, network_receiver_out) = unbounded();

        P2PService {
            swarm,
            network_receiver_in,
            network_sender_in,
            network_receiver_out,
            network_sender_out,
            network_name: network_name.to_owned(),
        }
    }

    /// Starts the libp2p service networking stack.
    pub async fn run(mut self) {
        let mut swarm_stream = self.swarm.fuse();
        let mut network_stream = self.network_receiver_in.fuse();
        let mut interval = stream::interval(Duration::from_secs(15)).fuse();

        loop {
            select! {
                swarm_event = swarm_stream.next() => match swarm_event {
                    // outbound events
                    Some(event) => match event {
                        SwarmEvent::Behaviour(ForestBehaviourEvent::PeerConnected(peer_id)) => {
                            debug!("Peer connected, {:?}", peer_id);
                            emit_event(&self.network_sender_out,
                                       NetworkEvent::PeerConnected(peer_id)).await;
                        }
                        SwarmEvent::Behaviour(ForestBehaviourEvent::PeerDisconnected(peer_id)) => {
                            emit_event(&self.network_sender_out, NetworkEvent::PeerDisconnected(peer_id)).await;
                        }
                    }
                },
                rpc_message = network_stream.next() => match rpc_message {
                    // Inbound messages
                    Some(message) => match message {}
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

/// Builds the transport stack that LibP2P will communicate over.
pub fn build_transport(local_key: Keypair) -> Boxed<(PeerId, StreamMuxerBox)> {
    TcpConfig::new()
        .upgrade(upgrade::Version::V1)
        .authenticate(NoiseConfig::xx(auth_keys).into_authenticated())
        .multiplex(mplex::MplexConfig::new())
        .boxed()
}
