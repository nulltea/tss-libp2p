use std::{
    error::Error,
};
use async_std::task;
use async_stream::stream;
use futures::{Sink, StreamExt};
use libp2p::{identity, mplex, Multiaddr, PeerId, Swarm, Transport};
use libp2p::{
    swarm::{SwarmBuilder, NetworkBehaviour},
    core::transport::upgrade,
    noise::{Keypair, X25519Spec, NoiseConfig},
    tcp::TokioTcpConfig,
};
use libp2p::swarm::SwarmEvent;
use round_based::{AsyncProtocol, Msg};
use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2020::state_machine::keygen::{Keygen, ProtocolMessage};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json;
use log::info;
use mpc_p2p::{P2PConfig, P2PService};


type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync + 'static>>;

#[tokio::main]
async fn main() -> Result<()> {
    pretty_env_logger::init();
    let keypair = identity::Keypair::generate_ed25519();

    let comm_serv = P2PService::new(keypair, P2PConfig::default(), "keygen");

    let p2p_task = task::spawn(async {
        comm_serv.run().await;
    });

    Ok(())
}
//
// pub async fn join_computation<M>(
//     mut swarm: Swarm<RoundBasedBehaviour>,
//     room_id: &str,
// ) -> Result<(
//     i64,
//     impl Stream<Item = Result<Msg<M>>>,
//     impl Sink<Msg<M>, Error = anyhow::Error>,
// )>
//     where
//         M: Serialize + DeserializeOwned,
// {
//     let incoming = stream! {
//         loop {
//             match swarm.select_next_some().await {
//                 SwarmEvent::Behaviour(event) => yield serde_json::from_str::<Msg<M>>(&msg).context("deserialize message"),
//                 _ => continue,
//             }
//         }
//     };
//
//     let outgoing = swarm.behaviour().broadcast_fn();
//
//     Ok((0, incoming, outgoing))
// }
