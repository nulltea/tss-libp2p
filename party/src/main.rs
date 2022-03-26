use std::error::Error;
use std::fs::File;
use std::io::Write;
use anyhow::anyhow;
use async_std::prelude::Stream;
use async_std::task;
use futures::{Sink, StreamExt};
use round_based::{AsyncProtocol, Msg};
use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2020::state_machine::keygen::{Keygen, ProtocolMessage};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json;
use log::info;
use mpc_p2p::{NetworkMessage, P2PConfig, NetworkWorker, WireMessage, WireRequest};
use mpc_p2p::NetworkEvent::BroadcastMessage;
use mpc_p2p::NetworkMessage::Message;


struct MPCMessage(PM);

enum PM {
    KeygenMessage(ProtocolMessage)
}

#[async_std::main]
async fn main() -> std::io::Result<()> {
    pretty_env_logger::init();

    let mut file = File::open("key.share").map_err(|e| anyhow!("failed to open file: {}", e))?;
    let keypair = identity::Keypair::generate_ed25519();

    let comm_serv = <NetworkWorker<MPCMessage>>::new(keypair, P2PConfig::default(), "keygen");

    let p2p_task = task::spawn(async {
        comm_serv.run().await;
    });

    let (index, incoming, outgoing) = join_computation(&comm_serv);

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

pub fn join_computation<M, T>(
    comm: &NetworkWorker<M>,
) -> Result<(
    i64,
    impl Stream<Item=Result<Msg<T>>>,
    impl Sink<Msg<T>, Error=anyhow::Error>,
)>
    where
        M: Serialize + DeserializeOwned,
        T: Serialize + DeserializeOwned,
{
    let incoming = comm.network_receiver().filter_map(|event| match event {
        BroadcastMessage(msg) => match msg.body {
            MPCMessage(PM::KeygenMessage(pm)) => Msg{
                sender: msg.sender,
                receiver: msg.receiver,
                body: pm,
            }
        },
        _ => None
    });

    let outgoing = futures::sink::unfold(comm, |comm, message: Msg<T>| async move {
        comm.network_sender().send(NetworkMessage::Broadcast(Msg{
            sender: message.sender,
            receiver: message.receiver,
            body: MPCMessage(PM::KeygenMessage(message.body))
        }));
        Ok::<_, anyhow::Error>(behaviour)
    });

    Ok((0, incoming, outgoing))
}
