#![feature(associated_type_defaults)]
#![feature(async_closure)]

mod echo;
mod error;
mod rt;
mod traits;

pub use error::*;
pub use rt::*;
pub use traits::*;

use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2020::state_machine::keygen;

pub enum ProtocolAgent {
    Keygen(Box<dyn ComputeAgent<StateMachine = keygen::Keygen> + Send>),
}
