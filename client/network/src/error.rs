use libp2p::{Multiaddr, PeerId};
use std::borrow::Cow;
use std::fmt;

/// Result type alias for the network.
pub type Result<T> = std::result::Result<T, Error>;

/// Error type for the network.
#[derive(derive_more::Display, derive_more::From)]
pub enum Error {
    /// Io error
    Io(std::io::Error),
    /// The same node (based on address) is registered with two different peer ids.
    #[display(
        fmt = "The same node (`{}`) is registered with two different peer ids: `{}` and `{}`",
        address,
        first_id,
        second_id
    )]
    DuplicateNode {
        /// The address of the node.
        address: Multiaddr,
        /// The first peer id that was found for the node.
        first_id: PeerId,
        /// The second peer id that was found for the node.
        second_id: PeerId,
    },
    /// The same request-response protocol has been registered multiple times.
    #[display(fmt = "Broadcast protocol registered multiple times: {}", protocol)]
    DuplicateBroadcastProtocol {
        /// Name of the protocol registered multiple times.
        protocol: Cow<'static, str>,
    },
}

// Make `Debug` use the `Display` implementation.
impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(ref err) => Some(err),
            Self::DuplicateNode { .. } | Self::DuplicateBroadcastProtocol { .. } => None,
        }
    }
}
