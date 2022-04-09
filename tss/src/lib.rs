pub mod client;
mod config;
mod keygen;

pub use config::*;
pub use keygen::*;

pub const KEYGEN_PROTOCOL_ID: &str = "/keygen/0.1.0";
pub const SIGN_PROTOCOL_ID: &str = "/sign/0.1.0";
