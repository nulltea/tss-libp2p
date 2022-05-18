#![feature(associated_type_defaults)]
#![feature(async_closure)]

mod coordination;
mod echo;
mod error;
mod negotiation;
mod network_proxy;
mod peerset;
mod runtime;
mod traits;

pub use error::*;
pub use runtime::*;
pub use traits::*;

use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2020::state_machine::keygen;

pub enum ProtocolAgent {
    Keygen(Box<dyn ComputeAgent<StateMachine = keygen::Keygen> + Send>),
}

pub trait ProtocolAgentFactory {
    fn make(&self, protocol_id: u64) -> Result<Box<dyn ComputeAgentAsync>>;
}
