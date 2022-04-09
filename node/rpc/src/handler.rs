use crate::RpcResult;
use anyhow::anyhow;
use curv::elliptic::curves::{Point, Secp256k1};
use jsonrpc_core::BoxFuture;
use jsonrpc_core_client::transports::ws;
use jsonrpc_derive::rpc;

#[rpc]
pub trait JsonRPCHandler {
    #[rpc(name = "keygen")]
    fn keygen(&self, t: u16, n: u16) -> BoxFuture<RpcResult<Point<Secp256k1>>>;

    #[rpc(name = "sign")]
    fn sign(&self, msg: Vec<u8>) -> BoxFuture<RpcResult<()>>;
}

pub async fn new_client(url: String) -> Result<gen_client::Client, anyhow::Error> {
    let cl = ws::connect::<gen_client::Client>(&url.parse().unwrap())
        .await
        .map_err(|e| anyhow!("node connection terminated w/ err: {:?}", e))?;

    Ok(cl)
}
