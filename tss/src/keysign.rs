use std::fs;

use std::io::Read;

use anyhow::anyhow;
use curv::arithmetic::Converter;
use curv::elliptic::curves::Secp256k1;
use curv::BigInt;

use futures::future::TryFutureExt;
use futures::StreamExt;
use futures_util::{pin_mut, FutureExt, SinkExt, TryStreamExt};

use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2020::state_machine::keygen::LocalKey;
use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2020::state_machine::sign::{
    OfflineStage, SignManual,
};
use round_based::{AsyncProtocol, Msg};

use mpc_runtime::{IncomingMessage, OutgoingMessage, Peerset};

pub struct KeySign {
    path: String,
}

#[async_trait::async_trait]
impl mpc_runtime::ComputeAgentAsync for KeySign {
    fn session_id(&self) -> u64 {
        0
    }

    fn protocol_id(&self) -> u64 {
        1
    }

    async fn compute(
        mut self: Box<Self>,
        mut parties: Peerset,
        args: Vec<u8>,
        rt_incoming: async_channel::Receiver<IncomingMessage>,
        rt_outgoing: async_channel::Sender<OutgoingMessage>,
    ) -> anyhow::Result<Vec<u8>> {
        parties.recover_from_cache().await?;
        let i = parties.index_of(parties.local_peer_id()).unwrap() + 1;
        let n = parties.len();
        let s_l = parties
            .parties_indexes
            .iter()
            .map(|i| (*i + 1) as u16)
            .collect();
        let local_key = self.read_local_key()?;

        let state_machine = OfflineStage::new(i, s_l, local_key)
            .map_err(|e| anyhow!("failed building state {e}"))?;

        let (incoming, outgoing) =
            crate::round_based::state_replication(rt_incoming.clone(), rt_outgoing.clone());

        let incoming = incoming.fuse();
        pin_mut!(incoming, outgoing);

        let completed_offline_stage = AsyncProtocol::new(state_machine, incoming, outgoing)
            .run()
            .await
            .map_err(|e| anyhow!("protocol execution terminated with error: {e}"))?;

        let (incoming, outgoing) = crate::round_based::state_replication(rt_incoming, rt_outgoing);
        pin_mut!(incoming, outgoing);

        let (signing, partial_signature) =
            SignManual::new(BigInt::from_bytes(&*args), completed_offline_stage)?;

        outgoing
            .send(Msg {
                sender: i,
                receiver: None,
                body: partial_signature,
            })
            .await
            .map_err(|_e| anyhow!("error sending partial signature"))?;

        let partial_signatures: Vec<_> = incoming
            .take(n - 1)
            .map_ok(|msg| msg.body)
            .try_collect()
            .await?;

        let sig = signing.complete(&partial_signatures)?;

        let signature_bytes = serde_ipld_dagcbor::to_vec(&sig)
            .map_err(|e| anyhow!("error encoding signature {e}"))?;

        Ok(signature_bytes)
    }
}

impl KeySign {
    pub fn new(p: &str) -> Self {
        Self { path: p.to_owned() }
    }

    fn read_local_key(&self) -> anyhow::Result<LocalKey<Secp256k1>> {
        let share_bytes =
            fs::read(self.path.as_str()).map_err(|e| anyhow!("error reading local key: {e}"))?;

        serde_json::from_slice(&share_bytes)
            .map_err(|e| anyhow!("error deserializing local key: {e}"))
    }
}
