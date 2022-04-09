use async_std::prelude::Stream;

use futures::channel::mpsc;
use futures::{Sink, StreamExt};

use log::info;
use mpc_p2p::broadcast::{IncomingMessage, OutgoingResponse};
use mpc_p2p::{broadcast, NetworkService};

use round_based::Msg;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::borrow::Cow;

use std::fmt::Debug;

pub async fn join_computation<M>(
    protocol_id: Cow<'static, str>,
    network_service: NetworkService,
    incoming_receiver: mpsc::Receiver<IncomingMessage>,
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
        (network_service.clone(), protocol_id.clone()),
        |(network_service, protocol_id), message: Msg<M>| async move {
            info!("outgoing message to {:?}", message);
            let payload = serde_ipld_dagcbor::to_vec(&message.body)?;

            if let Some(receiver_index) = message.receiver {
                network_service
                    .send_message(protocol_id.as_ref(), receiver_index - 1, payload)
                    .await;
            } else {
                network_service
                    .broadcast_message(protocol_id.as_ref(), payload)
                    .await;
            }

            Ok::<_, anyhow::Error>((network_service, protocol_id))
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
