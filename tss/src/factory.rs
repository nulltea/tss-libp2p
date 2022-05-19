use crate::DKG;
use mpc_runtime::ComputeAgentAsync;
use std::path::Path;

pub struct TssFactory {
    key_path: String,
}

impl TssFactory {
    pub fn new(key_path: String) -> Self {
        Self { key_path }
    }
}

impl mpc_runtime::ProtocolAgentFactory for TssFactory {
    fn make(&self, protocol_id: u64) -> mpc_runtime::Result<Box<dyn ComputeAgentAsync>> {
        match protocol_id {
            0 => Ok(Box::new(DKG::new(&self.key_path))),
            _ => Err(mpc_runtime::Error::UnknownProtocol(protocol_id)),
        }
    }
}
