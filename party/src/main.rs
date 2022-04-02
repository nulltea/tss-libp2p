use anyhow::anyhow;
use async_std::prelude::Stream;
use async_std::task;
use futures::channel::mpsc;
use futures::{pin_mut, Sink, StreamExt};
use libp2p::identity;
use mpc_p2p::{
    broadcast, NetworkConfig, NetworkService, NetworkWorker, Params, MASTER_PEERSET,
};
use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2020::state_machine::keygen::{
    Keygen, ProtocolMessage,
};
use round_based::{AsyncProtocol, Msg};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::error::Error;
use std::fs::File;
use std::io::Write;

struct MPCMessage(PM);

enum PM {
    KeygenMessage(ProtocolMessage),
}

#[async_std::main]
async fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();

    let mut file = File::open("key.share").map_err(|e| anyhow!("failed to open file: {}", e))?;
    let keypair = identity::Keypair::generate_ed25519();
    let peers = vec![];

    let (keygen_config, keygen_receiver) =
        broadcast::ProtocolConfig::new_with_receiver("keygen".into(), peers.len());

    let (net_worker, net_service) = {
        let network_config = NetworkConfig::new(peers.into_iter());
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


    let (
        index,
        incoming,
        outgoing
    ) = join_computation(net_service, keygen_receiver);

    let incoming = incoming.fuse();

    pin_mut!(incoming, outgoing);

    let keygen = Keygen::new(index, 2, 3)?;
    let output = AsyncProtocol::new(keygen, incoming, outgoing)
        .run()
        .await
        .map_err(|e| anyhow!("protocol execution terminated with error: {}", e))?;

    let share_bytes = serde_ipld_dagcbor::to_vec(&output)
        .map_err(|e| anyhow!("share serialization terminated with error: {}", e))?;

    file.write(&share_bytes)?;

    p2p_task.cancel();

    Ok(())
}

pub fn join_computation<M>(
    network_service: NetworkService,
    incoming_receiver: mpsc::Receiver<broadcast::IncomingMessage>,
) -> (
    u16,
    impl Stream<Item = Result<Msg<M>, anyhow::Error>>,
    impl Sink<Msg<M>, Error = anyhow::Error>,
) where M: Serialize + DeserializeOwned
{
    let index = network_service.local_peer_index();

    let outgoing = futures::sink::unfold(
        network_service.clone(),
        |network_service, message: Msg<M>| async move {
            let payload = serde_ipld_dagcbor::to_vec(&message.body)?; // todo: abstract serialization

            if let Some(receiver_index) = message.receiver {
                if let Some(receiver_peer) = network_service
                    .get_peerset()
                    .at_index(MASTER_PEERSET, receiver_index as usize)
                {
                    network_service.send_message("keygen", receiver_peer.clone(), payload);
                }
            } else {
                network_service.broadcast_message("keygen", payload)
            }

            Ok::<_, anyhow::Error>(network_service)
        },
    );

    let incoming = incoming_receiver.map(move |message: broadcast::IncomingMessage| {
        let body: M = serde_ipld_dagcbor::from_slice(&*message.payload)?;

        if let Some(known_sender_index) = network_service
            .get_peerset()
            .index_of(MASTER_PEERSET, message.peer)
        {
            Ok(Msg {
                sender: known_sender_index as u16,
                receiver: if message.is_broadcast {
                    None
                } else {
                    Some(index as u16)
                },
                body
            })
        } else {
            //  message.pending_response.send(OutgoingResponse::)
            Err(anyhow!("unknown sender "))
        }
    });

    (index as u16, incoming, outgoing)
}
