use crate::client::join_computation;
use anyhow::anyhow;

use curv::elliptic::curves::Secp256k1;
use futures::channel::mpsc;
use futures::future::{FutureExt, TryFutureExt};
use futures::{pin_mut, StreamExt};
use mpc_p2p::broadcast::IncomingMessage;
use mpc_p2p::NetworkService;
use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2020::state_machine::keygen::{
    Keygen, LocalKey,
};
use round_based::AsyncProtocol;
use std::borrow::Cow;

pub struct DKG {
    protocol_id: Cow<'static, str>,
    network_service: NetworkService,
    incoming_receiver: mpsc::Receiver<IncomingMessage>,
}

impl DKG {
    pub fn new(
        protocol_id: Cow<'static, str>,
        network_service: NetworkService,
        incoming_receiver: mpsc::Receiver<IncomingMessage>,
    ) -> Self {
        Self {
            protocol_id,
            network_service,
            incoming_receiver,
        }
    }

    pub async fn compute(self, t: u16, n: u16) -> anyhow::Result<LocalKey<Secp256k1>> {
        let (index, incoming, outgoing) = join_computation(
            self.protocol_id.clone(),
            self.network_service.clone(),
            self.incoming_receiver,
        )
        .await;

        let incoming = incoming.fuse();
        pin_mut!(incoming, outgoing);

        let keygen = Keygen::new(index + 1, t, n)?;
        let output = AsyncProtocol::new(keygen, incoming, outgoing)
            .run()
            .await
            .map_err(|e| anyhow!("protocol execution terminated with error: {e}"))?;

        Ok(output)
    }
}
