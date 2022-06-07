use crate::peerset::Peerset;
use crate::PeersetCacher;
use async_std::path::Path;
use mpc_p2p::RoomId;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};

#[derive(Default)]
pub struct EphemeralCacher {
    store: HashMap<RoomId, Peerset>,
}

impl PeersetCacher for EphemeralCacher {
    fn read_peerset(&self, room_id: &RoomId) -> Result<Peerset, Self> {
        match self.store.get(room_id) {
            Some(p) => Ok(p.clone()),
            None => Err(Self),
        }
    }

    fn write_peerset(&mut self, room_id: &RoomId, peerset: Peerset) -> Result<(), Self> {
        self.store
            .entry(room_id.clone())
            .and_modify(|e| *e = peerset.clone())
            .or_insert(peerset);
        Ok(())
    }
}

pub struct PersistentCacher {
    path: String,
}

impl PeersetCacher for PersistentCacher {
    fn read_peerset(&self, room_id: &RoomId) -> Result<Peerset, Self> {
        let mut file = File::open(format!("{}/{}", self.path, room_id.as_str()))
            .map_err(|e| anyhow!("error opening local key file: {e}"))?;

        let mut buf = vec![];
        file.read(&mut buf).map_err(|e| Self)?;

        serde_json::from_slice(&buf).map_err(|e| Self)
    }

    fn write_peerset(&mut self, room_id: &RoomId, peerset: Peerset) -> Result<(), Self> {
        let mut file = File::open(format!("{}/{}", self.path, room_id.as_str()))
            .map_err(|e| anyhow!("error opening local key file: {e}"))?;

        let bytes = serde_json::to_vec(&peerset).map_err(|e| Self)?;
        file.write(&bytes).map_err(|e| Self)?;
        Ok(())
    }
}

impl PersistentCacher {
    pub fn new(p: &str) -> Self {
        Self { path: p.to_owned() }
    }
}
