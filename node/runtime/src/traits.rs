use round_based::StateMachine;
use std::borrow::Cow;

pub trait ComputeAgent {
    type StateMachine: StateMachine;

    fn construct_state(&mut self, i: u16, n: u16) -> Self::StateMachine;

    fn protocol_id(&self) -> Cow<'static, str>;

    fn done(self: Box<Self>, result: anyhow::Result<<Self::StateMachine as StateMachine>::Output>);
}
