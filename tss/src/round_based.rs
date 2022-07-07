use anyhow::anyhow;
use futures::channel::oneshot;
use futures::{Sink, Stream};
use futures_util::{SinkExt, StreamExt};
use log::info;
use mpc_runtime::{IncomingMessage, MessageRouting, OutgoingMessage};
use round_based::Msg;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fmt::Debug;

pub(crate) fn state_replication<M>(
    incoming: async_channel::Receiver<IncomingMessage>,
    outgoing: async_channel::Sender<OutgoingMessage>,
) -> (
    impl Stream<Item = Result<Msg<M>, anyhow::Error>>,
    impl Sink<Msg<M>, Error = anyhow::Error>,
)
where
    M: Serialize + DeserializeOwned + Debug,
{
    let incoming = incoming.map(move |msg: IncomingMessage| {
        let body: M = serde_ipld_dagcbor::from_slice(&*msg.body).unwrap();

        Ok(Msg::<M> {
            sender: msg.from,
            receiver: match msg.to {
                MessageRouting::Broadcast => None,
                MessageRouting::PointToPoint(i) => Some(i),
            },
            body,
        })
    });

    let outgoing = futures::sink::unfold(outgoing, move |outgoing, message: Msg<M>| async move {
        let payload = serde_ipld_dagcbor::to_vec(&message.body).map_err(|e| anyhow!("{e}"))?;
        let (tx, rx) = oneshot::channel();
        outgoing
            .send(OutgoingMessage {
                body: payload,
                to: match message.receiver {
                    Some(remote_index) => MessageRouting::PointToPoint(remote_index),
                    None => MessageRouting::Broadcast,
                },
                sent: Some(tx),
            })
            .await
            .expect("channel is expected to be open");

        let _ = rx.await;

        Ok::<_, anyhow::Error>(outgoing)
    });

    (incoming, outgoing)
}
