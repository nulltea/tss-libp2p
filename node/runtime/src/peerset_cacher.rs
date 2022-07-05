use crate::peerset::Peerset;
use crate::PeersetCacher;
use anyhow::anyhow;
use async_std::path::{Path, PathBuf};
use libp2p::PeerId;
use mpc_p2p::RoomId;
use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::io::{Read, Write};

#[derive(Default)]
pub struct EphemeralCacher {
    store: HashMap<RoomId, Peerset>,
}

impl PeersetCacher for EphemeralCacher {
    fn read_peerset(&self, room_id: &RoomId) -> anyhow::Result<Peerset> {
        match self.store.get(room_id) {
            Some(p) => Ok(p.clone()),
            None => Err(anyhow!("no cache exists for room")),
        }
    }

    fn write_peerset(&mut self, room_id: &RoomId, peerset: Peerset) -> anyhow::Result<()> {
        self.store
            .entry(room_id.clone())
            .and_modify(|e| *e = peerset.clone())
            .or_insert(peerset);
        Ok(())
    }
}

pub struct PersistentCacher {
    local_peer_id: PeerId,
    path: PathBuf,
}

impl PeersetCacher for PersistentCacher {
    fn read_peerset(&self, room_id: &RoomId) -> anyhow::Result<Peerset> {
        let buf = fs::read(self.path.join(room_id.as_str()))
            .map_err(|e| anyhow!("error reading peerset cache file: {e}"))?;

        Ok(Peerset::from_bytes(&*buf, self.local_peer_id))
    }

    fn write_peerset(&mut self, room_id: &RoomId, peerset: Peerset) -> anyhow::Result<()> {
        let path = self.path.join(room_id.as_str());
        let dir = path.parent().unwrap();
        fs::create_dir_all(dir).unwrap();
        fs::write(path, peerset.to_bytes()).map_err(|e| anyhow!("error writing to file: {e}"))?;

        Ok(())
    }
}

impl PersistentCacher {
    pub fn new<P: AsRef<Path>>(p: P, local_peer_id: PeerId) -> Self {
        Self {
            path: PathBuf::from(p.as_ref()),
            local_peer_id,
        }
    }
}
