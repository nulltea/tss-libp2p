use crate::JsonRPCHandler;
use anyhow::anyhow;
use jsonrpc_core::IoHandler;
use jsonrpc_ws_server::{Server, ServerBuilder};

pub struct JsonRPCServer {
    server: Server,
}

impl JsonRPCServer {
    pub fn new<T>(config: Config, handler: T) -> Result<Self, anyhow::Error>
    where
        T: JsonRPCHandler,
    {
        let mut io = IoHandler::new();
        io.extend_with(handler.to_delegate());

        let server = ServerBuilder::new(io)
            .start(&config.host_address.parse().unwrap())
            .map_err(|e| anyhow!("json rpc server start terminated with err: {:?}", e))?;

        Ok(Self { server })
    }

    pub async fn run(self) -> Result<(), anyhow::Error> {
        self.server
            .wait()
            .map_err(|e| anyhow!("running json rpc server terminated with err: {:?}", e))
    }
}

pub struct Config {
    pub host_address: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host_address: "127.0.0.1:8080".to_string(),
        }
    }
}
