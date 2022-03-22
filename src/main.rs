#![feature(once_cell)]

mod network_behaviour;
mod mpc_protocol;

use std::{
    error::Error,
    lazy::Lazy,
};
use std::cell::RefCell;
use std::rc::Rc;
use anyhow::{anyhow, Context};
use tokio::prelude::*;
use tokio::stream::{Stream};
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

use crate::network_behaviour::RoundBasedBehaviour;

mod structs_proto {
    include!(concat!(env!("OUT_DIR"), "/structs.pb.rs"));
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync + 'static>>;

#[tokio::main]
async fn main() -> Result<()> {
    pretty_env_logger::init();
    let keys = identity::Keypair::generate_ed25519();
    let peer_id = PeerId::from(keys.public());

    info!("Peer Id: {}", peer_id.clone());

    let auth_keys = Keypair::<X25519Spec>::new()
        .into_authentic(&keys)
        .expect("can create auth keys");

    let transport = TokioTcpConfig::new()
        .upgrade(upgrade::Version::V1)
        .authenticate(NoiseConfig::xx(auth_keys).into_authenticated())
        .multiplex(mplex::MplexConfig::new())
        .boxed();

    let mut behaviour = RoundBasedBehaviour::new(vec![]);
    let mut swarm = Swarm::new(transport, behaviour, peer_id.clone());

    if let Some(addr) = std::env::args().nth(1) {
        swarm.listen_on(addr.parse()?)?;
    }

    if let Some(addr) = std::env::args().nth(2) {
        let remote: Multiaddr = addr.parse()?;
        //swarm.dial(remote)?;
        println!("Dialed {}", addr);
    };

    // let (index, incoming, outgoing) = join_computation(swarm, "/tss/keygen")
    //     .await
    //     .context("join computation")?;

    let swarm_cell = Rc::new(RefCell::new(swarm));
    let mut incoming_swarm = swarm_cell.borrow_mut();
    let mut outgoing_swarm = swarm_cell.borrow_mut();

    let incoming = stream! {
        loop {
            match incoming_swarm.select_next_some().await {
                SwarmEvent::Behaviour(event) => yield serde_json::from_str::<Msg<ProtocolMessage>>("").context("deserialize message"), // TODO event parsing
                _ => continue,
            }
        }
    };

    let outgoing = outgoing_swarm.behaviour_mut().broadcast_fn();

    let incoming = incoming.fuse();

    tokio::pin!(incoming);
    tokio::pin!(outgoing);

    let keygen = Keygen::new(1, 2, 2)?;

    let output = AsyncProtocol::new(keygen, incoming, outgoing)
        .run()
        .await
        .map_err(|e| anyhow!("protocol execution terminated with error: {}", e))?;
    let output = serde_json::to_string(&output).context("serialize output")?;

    println!("{}", output);

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
