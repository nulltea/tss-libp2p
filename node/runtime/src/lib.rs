#![feature(associated_type_defaults)]
#![feature(async_closure)]

mod coordination;
mod echo;
mod error;
mod execution;
mod negotiation;
mod network_proxy;
mod peerset;
mod runtime;
mod traits;

pub use error::*;
pub use runtime::*;
pub use traits::*;

pub trait ProtocolAgentFactory {
    fn make(&self, protocol_id: u64) -> Result<Box<dyn ComputeAgentAsync>>;
}
