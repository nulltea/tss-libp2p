use anyhow::anyhow;
use curv::elliptic::curves::{Point, Secp256k1};

use futures::future::TryFutureExt;
use futures::StreamExt;
use futures_util::{pin_mut, FutureExt};
use log::info;
use mpc_runtime::{IncomingMessage, OutgoingMessage, Peerset};
use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2020::state_machine::keygen::{
    Keygen, LocalKey,
};
use round_based::AsyncProtocol;
use std::fs::File;
use std::hash::Hasher;
use std::io::{BufReader, Write};
use std::path::Path;

pub struct KeyGen {
    path: String,
}

#[async_trait::async_trait]
impl mpc_runtime::ComputeAgentAsync for KeyGen {
    fn session_id(&self) -> u64 {
        0
    }

    fn protocol_id(&self) -> u64 {
        0
    }

    async fn compute(
        mut self: Box<Self>,
        mut parties: Peerset,
        args: Vec<u8>,
        incoming: async_channel::Receiver<IncomingMessage>,
        outgoing: async_channel::Sender<OutgoingMessage>,
    ) -> anyhow::Result<Vec<u8>> {
        let n = parties.len() as u16;
        let i = parties.index_of(parties.local_peer_id()).unwrap() + 1;
        let mut io = BufReader::new(&*args);
        let t = unsigned_varint::io::read_u16(&mut io).unwrap();

        let state_machine =
            Keygen::new(i, t, n).map_err(|e| anyhow!("failed building state {e}"))?;

        let (incoming, outgoing) = crate::round_based::state_replication(incoming, outgoing);

        let incoming = incoming.fuse();
        pin_mut!(incoming, outgoing);

        let res = AsyncProtocol::new(state_machine, incoming, outgoing)
            .run()
            .await
            .map_err(|e| anyhow!("protocol execution terminated with error: {e}"))?;

        let pk = self.save_local_key(res)?;
        parties.save_to_cache().await?;

        let pk_bytes = serde_ipld_dagcbor::to_vec(&pk)
            .map_err(|e| anyhow!("error encoding public key {e}"))?;

        Ok(pk_bytes)
    }
}

impl KeyGen {
    pub fn new(p: &str) -> Self {
        Self { path: p.to_owned() }
    }

    fn save_local_key(&self, local_key: LocalKey<Secp256k1>) -> anyhow::Result<Point<Secp256k1>> {
        let path = Path::new(self.path.as_str());
        let dir = path.parent().unwrap();
        std::fs::create_dir_all(dir).unwrap();

        let mut file = File::create(path)
            .map_err(|e| anyhow!("writing share to disk terminated with error: {e}"))?;

        let share_bytes = serde_json::to_vec(&local_key)
            .map_err(|e| anyhow!("share serialization terminated with error: {e}"))?;

        file.write(&share_bytes)
            .map_err(|e| anyhow!("error writing local key to file: {e}"))?;

        Ok(local_key.y_sum_s)
    }
}
