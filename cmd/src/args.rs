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

    #[options(help = "Setup args")]
    Setup(SetupArgs),
}

#[derive(Debug, Options, Clone)]
pub struct DeployArgs {
    help: bool,

    #[options(help = "path to parties config")]
    pub config_path: String,

    #[options(
        help = "path to setup directory (where secret key saved)",
        default = "./data/:id/"
    )]
    pub path: String,

    #[options(help = "peer discovery with Kad-DHT")]
    pub kademlia: bool,

    #[options(help = "peer discovery with mdns")]
    pub mdns: bool,
}

#[derive(Debug, Options, Clone)]
pub struct SetupArgs {
    help: bool,

    #[options(help = "libp2p multi address", default = "/ip4/127.0.0.1/tcp/4000")]
    pub multiaddr: String,

    #[options(help = "rpc address", default = "127.0.0.1:8080")]
    pub rpc_address: String,

    #[options(help = "path to configuration", default = "./config.json")]
    pub config_path: String,

    #[options(help = "path to setup artifacts", default = "./data/:id/")]
    pub path: String,
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

    #[options(help = "mpc room")]
    pub room: String,

    #[options(help = "json rpc addresses")]
    pub address: String,

    #[options(help = "threshold needed sign the messages (T)")]
    pub threshold: u16,

    #[options(help = "messages to sign")]
    pub messages: String,
}
