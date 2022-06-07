use anyhow::anyhow;
use curv::arithmetic::Converter;
use curv::elliptic::curves::{Point, Secp256k1};
use curv::BigInt;
use futures::channel::{mpsc, oneshot};
use futures::future::TryFutureExt;
use futures::StreamExt;
use futures_util::{pin_mut, FutureExt, SinkExt, TryStreamExt};
use mpc_runtime::{IncomingMessage, OutgoingMessage};
use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2020::state_machine::keygen::LocalKey;
use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2020::state_machine::sign::{
    OfflineStage, SignManual,
};
use round_based::{AsyncProtocol, Msg};
use std::fs::File;
use std::hash::Hasher;
use std::io::{BufReader, Read, Write};
use std::path::Path;

pub struct KeySign {
    path: String,
    done: Option<oneshot::Sender<anyhow::Result<Vec<u8>>>>,
}

#[async_trait::async_trait]
impl mpc_runtime::ComputeAgentAsync for KeySign {
    fn session_id(&self) -> u64 {
        0
    }

    fn protocol_id(&self) -> u64 {
        0
    }

    fn on_done(&mut self, done: oneshot::Sender<anyhow::Result<Vec<u8>>>) {
        let _ = self.done.insert(done);
    }

    async fn start(
        mut self: Box<Self>,
        i: u16,
        parties: Vec<u16>,
        args: Vec<u8>,
        rt_incoming: async_channel::Receiver<IncomingMessage>,
        rt_outgoing: async_channel::Sender<OutgoingMessage>,
    ) -> anyhow::Result<()> {
        let n = parties.len();
        let local_key = self.read_local_key()?;

        let state_machine = OfflineStage::new(i, parties, local_key)
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
            .map_err(|e| anyhow!("error sending partial signature"))?;

        let partial_signatures: Vec<_> = incoming
            .take(n - 1)
            .map_ok(|msg| msg.body)
            .try_collect()
            .await?;

        let sig = signing
            .complete(&partial_signatures)
            .context("online stage failed")?;

        if let Some(tx) = self.done.take() {
            let signature_bytes = serde_ipld_dagcbor::to_vec(&sig)
                .map_err(|e| anyhow!("error encoding signature {e}"))?;
            tx.send(signature_bytes)
                .expect("channel is expected to be open");
        }

        Ok(())
    }
}

impl KeySign {
    pub fn new(p: &str) -> Self {
        Self {
            path: p.to_owned(),
            done: None,
        }
    }

    fn read_local_key(&self) -> anyhow::Result<LocalKey<Secp256k1>> {
        let mut file = File::open(self.path.as_str())
            .map_err(|e| anyhow!("error opening local key file: {e}"))?;

        let mut share_bytes = vec![];
        file.read(&mut share_bytes)
            .map_err(|e| anyhow!("error reading local key: {e}"))?;

        serde_json::from_slice(&share_bytes)
            .map_err(|e| anyhow!("error deserializing local key: {e}"))
    }
}
