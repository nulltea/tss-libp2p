use anyhow::{anyhow, Context};
use async_std::prelude::Stream;
use async_std::task;
use futures::channel::mpsc;
use futures::{Sink, StreamExt};
use libp2p::identity;
use log::info;
use mpc_p2p::broadcast::OutgoingResponse;
use mpc_p2p::{
    broadcast, NetworkConfig, NetworkMessage, NetworkService, NetworkWorker, Params, MASTER_PEERSET,
};
use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2020::state_machine::keygen::{
    Keygen, ProtocolMessage,
};
use round_based::{AsyncProtocol, Msg};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json;
use std::error::Error;
use std::fs::File;
use std::io::Write;
use std::sync::mpsc::Receiver;

struct MPCMessage(PM);

enum PM {
    KeygenMessage(ProtocolMessage),
}

#[async_std::main]
async fn main() -> std::io::Result<()> {
    pretty_env_logger::init();

    let mut file = File::open("key.share").map_err(|e| anyhow!("failed to open file: {}", e))?;
    let keypair = identity::Keypair::generate_ed25519();
    let peers = vec![];

    let (keygen_config, keygen_receiver) =
        broadcast::ProtocolConfig::new_with_receiver("keygen".into(), peers.len());

    let (net_worker, net_service) = {
        let network_config = NetworkConfig::new(peers);
        let broadcast_protocols = vec![keygen_config];

        <NetworkWorker>::new(
            keypair,
            Params {
                network_config,
                broadcast_protocols,
            },
        )?
    };

    let p2p_task = task::spawn(async {
        net_worker.run().await;
    });

    let (index, incoming, outgoing) = join_computation(&net_service, keygen_receiver);

    let incoming = incoming.fuse();

    let keygen = Keygen::new(index, 2, 3)?;
    let output = AsyncProtocol::new(keygen, incoming, outgoing)
        .run()
        .await
        .map_err(|e| anyhow!("protocol execution terminated with error: {}", e))?;

    file.write(serde_ipld_dagcbor::to_vec(&output)?)?;

    p2p_task.cancel();

    Ok(())
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync + 'static>>;

pub fn join_computation<M>(
    network_service: &NetworkService,
    incoming_receiver: mpsc::Receiver<broadcast::IncomingMessage>,
) -> Result<(
    u16,
    impl Stream<Item = Result<Msg<M>>>,
    impl Sink<Msg<M>, Error = anyhow::Error>,
)>
where
    M: Serialize + DeserializeOwned,
    T: Serialize + DeserializeOwned,
{
    let local_index = network_service.local_peer_index();

    let incoming = incoming_receiver.map(|message: broadcast::IncomingMessage| {
        if let Some(known_sender_index) = network_service
            .get_peerset()
            .index_of(MASTER_PEERSET, message.peer)
        {
            Msg {
                sender: known_sender_index.as_u16(),
                receiver: if message.is_broadcast {
                    None
                } else {
                    Some(local_index.as_u16())
                },
                body: || -> M { serde_ipld_dagcbor::from_slice(&*message.payload) },
            }
        } else {
            //  message.pending_response.send(OutgoingResponse::)
        }
    });

    let outgoing = futures::sink::unfold(
        network_service,
        |network_service, message: Msg<T>| async move {
            let payload = serde_ipld_dagcbor::to_vec(message.body)?; // todo: abstract serialization

            if let Some(receiver_index) = message.receiver {
                if let Some(receiver_peer) = network_service
                    .get_peerset()
                    .at_index(MASTER_PEERSET, receiver_index.as_usize())
                {
                    network_service.send_message("keygen", receiver_peer.clone(), payload);
                }
            } else {
                network_service.broadcast_message("keygen", payload)
            }

            Ok::<_, anyhow::Error>(behaviour)
        },
    );

    Ok((local_index.as_u16(), incoming, outgoing))
}
