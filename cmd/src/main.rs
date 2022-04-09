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
use mpc_p2p::{broadcast, NetworkConfig, NetworkWorker, NodeKeyConfig, Params, Secret};
use mpc_rpc::server::JsonRPCServer;
use mpc_tss::{generate_config, Config, DKG, KEYGEN_PROTOCOL_ID};

use sha3::Digest;
use std::borrow::Cow;
use std::error::Error;

use std::process;

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

    let (keygen_config, keygen_receiver) = broadcast::ProtocolConfig::new_with_receiver(
        KEYGEN_PROTOCOL_ID.into(),
        config.parties.len() - 1,
    );

    let (net_worker, net_service) = {
        let _local_party_addr = local_party.network_peer.multiaddr.clone();
        let network_peers = config.parties.into_iter().map(|p| p.network_peer);
        let network_config =
            NetworkConfig::new(local_party.clone().network_peer.multiaddr, network_peers);

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

    let rpc_server = {
        let handler = RpcApi::new(DKG::new(
            Cow::Borrowed(KEYGEN_PROTOCOL_ID),
            net_service.clone(),
            keygen_receiver,
        ));
        JsonRPCServer::new(
            mpc_rpc::server::Config {
                host_address: local_party.rpc_addr,
            },
            handler,
        )
        .map_err(|e| anyhow!("json rpc server terminated with err: {}", e))?
    };

    rpc_server.run().await;

    let _ = p2p_task.cancel().await;

    println!("bye");

    Ok(())
}

async fn keygen(args: KeygenArgs) -> anyhow::Result<()> {
    let mut futures = vec![];
    for addr in args.addresses {
        futures.push(
            mpc_rpc::new_client(addr)
                .await?
                .keygen(args.threshold, args.number_of_parties),
        );
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
            Err(e) => error!("received error: {}", e),
        }
    }

    if let Some(address) = pub_key_hash {
        println!("Keygen finished! Keccak256 address => 0x{:x}", address);
    }

    Ok(())
}
