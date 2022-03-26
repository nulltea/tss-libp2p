#![feature(async_await)]

use serde_ipld_dagcbor;
use serde::{Deserialize, Serialize};
use futures::{prelude::*, AsyncRead, AsyncWrite};
use libp2p::{
    core::{
        upgrade::{read_length_prefixed, write_length_prefixed},
        ProtocolName,
    },
    request_response::RequestResponseCodec,
};
use std::io;


#[derive(Debug, Clone)]
pub struct MPCProtocol();
#[derive(Clone)]
pub struct MPCCodec();

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum WireRequest {
    Message(data),
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum WireResponse {
    Ack,
}

impl ProtocolName for MPCProtocol {
    fn protocol_name(&self) -> &[u8] {
        b"/tss/keygen"
    }
}

#[async_trait]
impl RequestResponseCodec for MPCCodec {
    type Protocol = MPCProtocol;
    type Request = WireRequest;
    type Response = WireResponse;

    async fn read_request<T>(
        &mut self,
        _: &MPCProtocol,
        io: &mut T,
    ) -> io::Result<Self::Request>
        where
            T: AsyncRead + Unpin + Send,
    {
        read_length_prefixed(io, 1024)
            .map(|req| match req {
                Ok(bytes) => {
                    serde_ipld_dagcbor::from_slice(bytes)?
                }
                Err(e) => Err(io::Error::new(io::ErrorKind::InvalidData, e)),
            })
            .await
    }

    async fn read_response<T>(
        &mut self,
        _: &MPCProtocol,
        io: &mut T,
    ) -> io::Result<Self::Response>
        where
            T: AsyncRead + Unpin + Send,
    {
        read_length_prefixed(io, 1024)
            .map(|res| match res {
                Ok(bytes) => {
                    serde_ipld_dagcbor::from_slice(bytes)?
                }
                Err(e) => Err(io::Error::new(io::ErrorKind::InvalidData, e)),
            })
            .await
    }

    async fn write_request<T>(
        &mut self,
        _: &MPCProtocol,
        io: &mut T,
        req: WireRequest,
    ) -> io::Result<()>
        where
            T: AsyncWrite + Unpin + Send,
    {
        write_length_prefixed(io, serde_ipld_dagcbor::to_vec(&req)).await
    }

    async fn write_response<T>(
        &mut self,
        _: &MPCProtocol,
        io: &mut T,
        res: WireResponse,
    ) -> io::Result<()>
        where
            T: AsyncWrite + Unpin + Send,
    {
        write_length_prefixed(io, serde_ipld_dagcbor::to_vec(req)).await
    }
}
