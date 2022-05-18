use mpc_p2p::broadcast::RequestFailure;
use std::fmt::{Display, Formatter};

pub enum Error {
    Busy,
    InconsistentEcho(u16),
    EchoFailed(RequestFailure),
    UnknownProtocol(u64),
    InternalError(anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Busy => write!(f, "protocol is already in computation"),
            Error::InconsistentEcho(i) => {
                write!(f, "inconsistent echo broadcast caused by party: {i}")
            }
            Error::EchoFailed(e) => write!(f, "echo broadcast terminated with error: {e}"),
            Error::UnknownProtocol(protocol_id) => {
                write!(f, "unknown protocol with id: {protocol_id}")
            }
            Error::InternalError(e) => {
                write!(f, "internal error occurred: {e}")
            }
        }
    }
}
