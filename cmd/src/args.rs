use gumdrop::Options;

#[derive(Debug, Options, Clone)]
pub struct MPCArgs {
    help: bool,
    #[options(command)]
    pub command: Option<Command>,
}

#[derive(Debug, Options, Clone)]
pub enum Command {
    #[options(help = "Deploy MPC daemon")]
    Deploy(DeployArgs),

    #[options(help = "Keygen args")]
    Keygen(KeygenArgs),

    #[options(help = "Sign args")]
    Sign(SignArgs),
}

#[derive(Debug, Options, Clone)]
pub struct DeployArgs {
    help: bool,

    #[options(help = "path to participation private_key")]
    pub private_key: String,

    #[options(help = "peer discovery with Kad-DHT")]
    pub kademlia: bool,

    #[options(help = "peer discovery with mdns")]
    pub mdns: bool,
}

#[derive(Debug, Options, Clone)]
pub struct KeygenArgs {
    help: bool,

    #[options(help = "mpc room")]
    pub room: String,

    #[options(help = "json rpc addresses")]
    pub address: String,

    #[options(help = "threshold number (T)")]
    pub threshold: u16,

    #[options(help = "number of parties (N)")]
    pub number_of_parties: u16,
}

#[derive(Debug, Options, Clone)]
pub struct SignArgs {
    help: bool,
}
