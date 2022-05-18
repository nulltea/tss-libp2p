use futures::channel::{mpsc, oneshot};

use round_based::StateMachine;

pub trait ComputeAgent: Send {
    type StateMachine: StateMachine;

    fn construct_state(&mut self, i: u16, n: u16) -> Self::StateMachine;

    fn session_id(&self) -> u64;

    fn done(self: Box<Self>, result: anyhow::Result<<Self::StateMachine as StateMachine>::Output>);
}

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
        n: u16,
        incoming: mpsc::Receiver<IncomingMessage>,
        outgoing: mpsc::Sender<OutgoingMessage>,
    ) -> anyhow::Result<()>;
}
