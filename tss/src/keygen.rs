use anyhow::anyhow;

use curv::elliptic::curves::{Point, Secp256k1};

use futures::future::TryFutureExt;

use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2020::state_machine::keygen::{
    Keygen, LocalKey,
};

use std::borrow::Cow;
use std::fs::File;
use std::hash::Hasher;
use std::io::Write;
use std::path::Path;

use tokio::sync::oneshot;

pub struct DKG {
    t: u16,
    p: String,
    i: Option<u16>,
    out: oneshot::Sender<anyhow::Result<Point<Secp256k1>>>,
}

impl mpc_runtime::ComputeAgent for DKG {
    type StateMachine = Keygen;

    fn construct_state(&mut self, i: u16, n: u16) -> Keygen {
        self.i = Some(i);
        Keygen::new(i, self.t, n).unwrap()
    }

    fn session_id(&self) -> u64 {
        0
    }

    fn done(self: Box<Self>, result: anyhow::Result<LocalKey<Secp256k1>>) {
        match result {
            Ok(local_key) => {
                let pub_key = self.save_local_key(local_key);
                self.out.send(pub_key);
            }
            Err(e) => {
                self.out.send(Err(e));
            }
        };
    }
}

impl DKG {
    pub fn new(t: u16, p: String) -> (Self, oneshot::Receiver<anyhow::Result<Point<Secp256k1>>>) {
        let (tx, rx) = oneshot::channel();
        let agent = Self {
            t,
            p,
            i: None,
            out: tx,
        };

        (agent, rx)
    }

    fn save_local_key(&self, local_key: LocalKey<Secp256k1>) -> anyhow::Result<Point<Secp256k1>> {
        let _i = self
            .i
            .expect("party index expected to be known by this point");

        let path_format = self.p.replace("{}", self.i.unwrap().to_string().as_str());
        let path = Path::new(path_format.as_str());
        let dir = path.parent().unwrap();
        std::fs::create_dir_all(dir).unwrap();

        let mut file = File::create(path)
            .map_err(|e| anyhow!("writing share to disk terminated with error: {e}"))?;

        let share_bytes = serde_json::to_vec(&local_key)
            .map_err(|e| anyhow!("share serialization terminated with error: {e}"))?;

        file.write(&share_bytes)
            .map_err(|e| anyhow!("share serialization terminated with error: {e}"))?;

        Ok(local_key.y_sum_s)
    }
}
