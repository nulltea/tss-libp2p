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
use mpc_p2p::{broadcast, NetworkWorker, NodeKeyConfig, Params, RoomArgs, Secret};
use mpc_rpc::server::JsonRPCServer;
use mpc_runtime::RuntimeDaemon;
use mpc_tss::{generate_config, Config, DKG};
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

    let (room_id, room_cfg, room_rx) =
        RoomArgs::new_full("tss/0".to_string(), boot_peers, config.parties.len());

    let (net_worker, net_service) = {
        let cfg = Params {
            listen_address: local_party.network_peer.multiaddr.clone(),
            rooms: vec![room_cfg],
            mdns: args.mdns,
            kademlia: args.kademlia,
        };

        NetworkWorker::new(node_key, cfg)?
    };

    let net_task = task::spawn(async {
        net_worker.run().await;
    });

    let (rt_worker, rt_service) = RuntimeDaemon::new(
        net_service,
        iter::once((room_id, room_rx)),
        iter::once((0, DKG::new(2, "data/player_{}/key.share".to_string()))),
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
    let res = mpc_rpc::new_client(args.address)
        .await?
        .keygen(args.room, args.number_of_parties, args.threshold)
        .await;

    let pub_key_hash = match res {
        Ok(pub_key) => {
            let hash = sha3::Keccak256::digest(pub_key.to_bytes(true).to_vec());
            if matches!(pub_key_hash, Some(pk_hash) if pk_hash != hash) {
                return Err(anyhow!(
                    "received inconsistent public key (prev={:x}, new={:x})",
                    pub_key_hash.unwrap(),
                    hash
                ));
            } else {
                sha3::Keccak256::digest(pub_key.to_bytes(true).to_vec())
            }
        }
        Err(e) => {
            return Err(anyhow!("received error: {}", e));
        }
    };

    println!("Keygen finished! Keccak256 address => 0x{:x}", pub_key_hash);

    Ok(())
}
