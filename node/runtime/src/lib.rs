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

#[derive(Clone)]
pub enum ProtocolAgent {
    Keygen(Box<dyn ComputeAgent<StateMachine = keygen::Keygen> + Send + Clone>),
}

impl ProtocolAgent {
    pub fn unwrap<SM>(self) -> Box<dyn ComputeAgent<StateMachine = SM> + Send + Clone> {
        match self {
            ProtocolAgent::Keygen(a) => a,
        }
    }

    pub fn unwrap_ref<SM>(&self) -> &Box<dyn ComputeAgent<StateMachine = SM> + Send + Clone> {
        match self {
            ProtocolAgent::Keygen(a) => a,
        }
    }

    pub fn clone_inner<SM>(&mut self) -> Box<dyn ComputeAgent<StateMachine = SM> + Send + Clone> {
        match self {
            ProtocolAgent::Keygen(a) => Box::new(a.clone()),
        }
    }
}
