use anyhow::anyhow;
use futures::channel::mpsc;
use futures::{Sink, Stream};
use futures_util::{SinkExt, StreamExt};
use mpc_runtime::{IncomingMessage, MessageRouting, OutgoingMessage};
use round_based::Msg;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fmt::Debug;

pub(crate) fn state_replication<M>(
    incoming: mpsc::Receiver<IncomingMessage>,
    outgoing: mpsc::Sender<OutgoingMessage>,
) -> (
    impl Stream<Item = Result<Msg<M>, anyhow::Error>>,
    impl Sink<Msg<M>, Error = anyhow::Error>,
)
where
    M: Serialize + DeserializeOwned + Debug,
{
    let incoming = incoming.map(move |msg: mpc_runtime::IncomingMessage| {
        Ok(Msg::<M> {
            sender: msg.from,
            receiver: match msg.to {
                MessageRouting::Broadcast => None,
                MessageRouting::PointToPoint(i) => Some(i),
            },
            body: serde_ipld_dagcbor::from_slice(&*msg.body).unwrap(),
        })
    });

    let outgoing =
        futures::sink::unfold(outgoing, move |mut outgoing, message: Msg<M>| async move {
            let payload = serde_ipld_dagcbor::to_vec(&message.body).map_err(|e| anyhow!("{e}"))?;

            outgoing
                .send(mpc_runtime::OutgoingMessage {
                    body: payload,
                    to: match message.receiver {
                        Some(remote_index) => MessageRouting::PointToPoint(remote_index),
                        None => MessageRouting::Broadcast,
                    },
                })
                .await;

            Ok::<_, anyhow::Error>(outgoing)
        });

    (incoming, outgoing)
}
