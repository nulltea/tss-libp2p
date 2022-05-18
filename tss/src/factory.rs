use crate::DKG;
use mpc_runtime::ComputeAgentAsync;

pub struct TssFactory;

impl mpc_runtime::ProtocolAgentFactory for TssFactory {
    fn make(&self, protocol_id: u64) -> mpc_runtime::Result<Box<dyn ComputeAgentAsync>> {
        match protocol_id {
            0 => Ok(Box::new(DKG::new(0, "data/player_{}/key.share"))),
            _ => Err(mpc_runtime::Error::UnknownProtocol(protocol_id)),
        }
    }
}
