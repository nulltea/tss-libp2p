use crate::peerset::Peerset;
use anyhow::anyhow;
use futures::channel::oneshot;
use mpc_p2p::RoomId;
use std::collections::HashSet;

pub struct IncomingMessage {
    /// Index of party who sent the message.
    pub from: u16,

    /// Message sent by the remote.
    pub body: Vec<u8>,

    pub to: MessageRouting,
}

pub struct OutgoingMessage {
    /// Message sent by the remote.
    pub body: Vec<u8>,

    pub to: MessageRouting,
}

#[derive(Copy, Clone, Debug)]
pub enum MessageRouting {
    Broadcast,
    PointToPoint(u16),
}

#[async_trait::async_trait]
pub trait ComputeAgentAsync: Send + Sync {
    fn session_id(&self) -> u64;

    fn protocol_id(&self) -> u64;

    fn on_done(&mut self, done: oneshot::Sender<anyhow::Result<Vec<u8>>>);

    async fn start(
        self: Box<Self>,
        i: u16,
        parties: Vec<u16>,
        args: Vec<u8>,
        incoming: async_channel::Receiver<IncomingMessage>,
        outgoing: async_channel::Sender<OutgoingMessage>,
    ) -> anyhow::Result<()>;
}

pub trait PeersetCacher {
    fn read_peerset(&self, room_id: &RoomId) -> anyhow::Result<Peerset>;

    fn write_peerset(&mut self, room_id: &RoomId, peerset: Peerset) -> anyhow::Result<()>;
}
