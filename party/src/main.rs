mod config;

use crate::config::{Config, PartyConfig};
use anyhow::anyhow;
use async_std::prelude::Stream;
use async_std::task;
use futures::channel::mpsc;
use futures::{pin_mut, Sink, StreamExt};
use libp2p::{Multiaddr, PeerId};
use log::info;
use mpc_p2p::broadcast::OutgoingResponse;
use mpc_p2p::{
    broadcast, MultiaddrWithPeerId, NetworkConfig, NetworkService, NetworkWorker, NodeKeyConfig,
    Params, Secret,
};
use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2020::state_machine::keygen::Keygen;
use round_based::{AsyncProtocol, Msg};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::error::Error;
use std::fmt::Debug;
use std::fs::File;
use std::io::Write;
use std::str::FromStr;
use std::{env, fs, io};

const KEYGEN_PROTOCOL_ID: &str = "/keygen/0.1.0";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();
    let party_index = env::args()
        .nth(1)
        .expect("party index argument is required");
    let mut file = File::create(format!("data/key{party_index}.share"))
        .map_err(|e| anyhow!("failed to open file: {}", e))?;
    let node_key = NodeKeyConfig::Ed25519(Secret::File(format!("data/{party_index}.key").into()));

    let local_peer_id = PeerId::from(node_key.clone().into_keypair()?.public());

    let config = Config::load_config("config.json").or_else(|_| generate_config(3))?;
    let local_party_addr = config.addr_of_peer_id(local_peer_id).unwrap();
    let parties_count = config.parties.len();

    let (keygen_config, keygen_receiver) = broadcast::ProtocolConfig::new_with_receiver(
        KEYGEN_PROTOCOL_ID.into(),
        config.parties.len() - 1,
    );

    let (net_worker, net_service) = {
        let network_peers = config.parties.into_iter().map(|p| p.network_peer);
        let network_config = NetworkConfig::new(local_party_addr, network_peers);

        for peer in network_config.initial_peers.iter() {
            println!("peer: {}", peer)
        }

        let broadcast_protocols = vec![keygen_config];

        NetworkWorker::new(
            node_key,
            Params {
                network_config,
                broadcast_protocols,
            },
        )?
    };

    let p2p_task = task::spawn(async {
        net_worker.run().await;
    });

    let (index, incoming, outgoing) = join_computation(net_service, keygen_receiver).await;

    let incoming = incoming.fuse();

    pin_mut!(incoming, outgoing);

    println!(
        "local party index: {} parties count {}",
        index + 1,
        parties_count
    );

    println!("press any key to start keygen");
    let mut s = String::new();
    io::stdin().read_line(&mut s)?;
    let keygen = Keygen::new(index + 1, 1, parties_count as u16)?;
    let output = AsyncProtocol::new(keygen, incoming, outgoing)
        .run()
        .await
        .map_err(|e| anyhow!("protocol execution terminated with error: {e}"))?;

    let share_bytes = serde_json::to_vec(&output)
        .map_err(|e| anyhow!("share serialization terminated with error: {e}"))?;

    file.write(&share_bytes)?;

    let _ = p2p_task.cancel().await;

    Ok(())
}

pub async fn join_computation<M>(
    network_service: NetworkService,
    incoming_receiver: mpsc::Receiver<broadcast::IncomingMessage>,
) -> (
    u16,
    impl Stream<Item = Result<Msg<M>, anyhow::Error>>,
    impl Sink<Msg<M>, Error = anyhow::Error>,
)
where
    M: Serialize + DeserializeOwned + Debug,
{
    let index = network_service.local_peer_index().await;

    let outgoing = futures::sink::unfold(
        network_service.clone(),
        |network_service, message: Msg<M>| async move {
            info!("outgoing message to {:?}", message);
            let payload = serde_ipld_dagcbor::to_vec(&message.body)?; // todo: abstract serialization

            if let Some(receiver_index) = message.receiver {
                network_service
                    .send_message(KEYGEN_PROTOCOL_ID.into(), receiver_index - 1, payload)
                    .await;
            } else {
                network_service
                    .broadcast_message(KEYGEN_PROTOCOL_ID.into(), payload)
                    .await;
            }

            Ok::<_, anyhow::Error>(network_service)
        },
    );

    let incoming = incoming_receiver.map(move |message: broadcast::IncomingMessage| {
        let body: M = serde_ipld_dagcbor::from_slice(&*message.payload)?;
        message.pending_response.send(OutgoingResponse {
            result: Ok(vec![]),
            sent_feedback: None,
        });
        info!(
            "incoming message from {} => {:?}",
            message.peer_index + 1,
            body
        );

        Ok(Msg {
            sender: message.peer_index + 1,
            receiver: if message.is_broadcast {
                None
            } else {
                Some(index + 1)
            },
            body,
        })
    });

    (index as u16, incoming, outgoing)
}

fn generate_config(n: u32) -> Result<Config, anyhow::Error> {
    let mut parties = vec![];

    for i in 0..n {
        let node_key = NodeKeyConfig::Ed25519(Secret::File(format!("data/{i}.key").into()));
        let keypair = node_key
            .into_keypair()
            .map_err(|e| anyhow!("keypair generating err: {}", e))?;
        let peer_id = PeerId::from(keypair.public());
        let multiaddr = Multiaddr::from_str(format!("/ip4/127.0.0.1/tcp/400{i}").as_str())
            .map_err(|e| anyhow!("multiaddr parce err: {}", e))?;
        let network_peer = MultiaddrWithPeerId { multiaddr, peer_id };

        parties.push(PartyConfig {
            name: format!("player_{i}"),
            network_peer,
        })
    }

    let config = Config { parties };

    let json_bytes = serde_json::to_vec(&config)
        .map_err(|e| anyhow!("config encoding terminated with err: {}", e))?;

    fs::write("config.json", json_bytes.as_slice())
        .map_err(|e| anyhow!("writing config err: {}", e))?;

    Ok(config)
}
