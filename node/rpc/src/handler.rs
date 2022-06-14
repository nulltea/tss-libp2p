use crate::RpcResult;
use anyhow::anyhow;
use curv::elliptic::curves::{Point, Secp256k1};
use jsonrpc_core::BoxFuture;
use jsonrpc_core_client::transports::ws;
use jsonrpc_derive::rpc;
use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2020::party_i::SignatureRecid;

#[rpc]
pub trait JsonRPCHandler {
    #[rpc(name = "keygen")]
    fn keygen(&self, room: String, n: u16, t: u16) -> BoxFuture<RpcResult<Point<Secp256k1>>>;

    #[rpc(name = "sign")]
    fn sign(&self, room: String, t: u16, msg: Vec<u8>) -> BoxFuture<RpcResult<SignatureRecid>>;
}

pub async fn new_client(url: String) -> Result<gen_client::Client, anyhow::Error> {
    let cl = ws::connect::<gen_client::Client>(&url.parse().unwrap())
        .await
        .map_err(|e| anyhow!("node connection terminated w/ err: {:?}", e))?;

    Ok(cl)
}
