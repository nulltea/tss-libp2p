#![feature(async_closure)]

mod args;

use crate::args::{Command, DeployArgs, KeygenArgs, MPCArgs};
use anyhow::anyhow;
use async_std::task;
use futures::future::{FutureExt, TryFutureExt};
use futures::StreamExt;
use gumdrop::Options;
use libp2p::PeerId;
use log::error;
use mpc_api::RpcApi;
use mpc_p2p::{broadcast, NetworkConfig, NetworkWorker, NodeKeyConfig, Params, RoomConfig, Secret};
use mpc_peerset::{Peerset, PeersetConfig, SetConfig};
use mpc_rpc::server::JsonRPCServer;
use mpc_runtime::RuntimeDaemon;
use mpc_tss::{generate_config, Config, KEYGEN_PROTOCOL_ID};
use sha3::Digest;
use std::borrow::Cow;
use std::error::Error;
use std::{iter, process};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();

    let args: MPCArgs = MPCArgs::parse_args_default_or_exit();
    let command = args.command.unwrap_or_else(|| {
        eprintln!("[command] is required");
        eprintln!("{}", MPCArgs::usage());
        process::exit(2)
    });

    match command {
        Command::Deploy(args) => deploy(args).await?,
        Command::Keygen(args) => keygen(args).await?,
        Command::Sign(_sign_args) => println!("Sign"),
    }

    Ok(())
}

async fn deploy(args: DeployArgs) -> Result<(), anyhow::Error> {
    let node_key = NodeKeyConfig::Ed25519(Secret::File(args.private_key.into()).into());
    let local_peer_id = PeerId::from(node_key.clone().into_keypair()?.public());

    let config = Config::load_config("config.json").or_else(|_| generate_config(3))?;
    let local_party = config
        .party_by_peer_id(local_peer_id.clone())
        .unwrap()
        .clone();

    let boot_peers: Vec<_> = config
        .parties
        .iter()
        .map(|p| p.network_peer.clone())
        .collect();

    let (peerset, peerset_handle) = {
        Peerset::from_config(PeersetConfig {
            sets: vec![SetConfig::new(
                boot_peers.clone(),
                config.parties.len() as u32,
            )],
        })
    };

    let (keygen_config, keygen_receiver) = broadcast::ProtocolConfig::new_with_receiver(
        KEYGEN_PROTOCOL_ID.into(),
        config.parties.len() - 1,
    );

    let (net_worker, net_service) = {
        let network_config = NetworkConfig {
            listen_address: local_party.network_peer.multiaddr.clone(),
            rooms: vec![RoomConfig {
                name: "tss/0".to_string(),
                set: 0,
                boot_peers,
            }],
            mdns: args.mdns,
            kademlia: args.kademlia,
        };

        let broadcast_protocols = vec![keygen_config];

        NetworkWorker::new(
            node_key,
            peerset,
            Params {
                network_config,
                broadcast_protocols,
            },
        )?
    };

    let net_task = task::spawn(async {
        net_worker.run().await;
    });

    let (rt_worker, rt_service) = RuntimeDaemon::new(
        net_service,
        peerset_handle,
        vec![((Cow::Borrowed(KEYGEN_PROTOCOL_ID), keygen_receiver))].into_iter(),
    );

    let rt_task = task::spawn(async {
        rt_worker.run().await;
    });

    let rpc_server = {
        let handler = RpcApi::new(rt_service);
        JsonRPCServer::new(
            mpc_rpc::server::Config {
                host_address: local_party.rpc_addr,
            },
            handler,
        )
        .map_err(|e| anyhow!("json rpc server terminated with err: {}", e))?
    };

    rpc_server.run().await;

    let _ = rt_task.cancel().await;
    let _ = net_task.cancel().await;

    println!("bye");

    Ok(())
}

async fn keygen(args: KeygenArgs) -> anyhow::Result<()> {
    let mut futures = vec![];
    for addr in args.addresses {
        futures.push(mpc_rpc::new_client(addr).await?.keygen(args.threshold));
    }

    let mut pub_key_hash = None;

    for res in futures.into_iter() {
        match res.await {
            Ok(pub_key) => {
                let hash = sha3::Keccak256::digest(pub_key.to_bytes(true).to_vec());
                if matches!(pub_key_hash, Some(pk_hash) if pk_hash != hash) {
                    error!(
                        "received inconsistent public key (prev={:x}, new={:x})",
                        pub_key_hash.unwrap(),
                        hash
                    )
                } else {
                    pub_key_hash = Some(sha3::Keccak256::digest(pub_key.to_bytes(true).to_vec()))
                }
            }
            Err(e) => {
                return Err(anyhow!("received error: {}", e));
            }
        }
    }

    if let Some(address) = pub_key_hash {
        println!("Keygen finished! Keccak256 address => 0x{:x}", address);
    }

    Ok(())
}
