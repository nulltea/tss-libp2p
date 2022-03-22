#![feature(async_await)]

use crate::structs_proto as proto;
use async_trait::async_trait;
use futures::{prelude::*, AsyncRead, AsyncWrite};
use libp2p::{
    core::{
        upgrade::{read_length_prefixed, write_length_prefixed},
        ProtocolName,
    },
    request_response::RequestResponseCodec,
};
use prost::Message;
use std::io;
use tokio::prelude::*;

#[derive(Debug, Clone)]
pub struct MPCProtocol();
#[derive(Clone)]
pub struct MPCCodec();

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MPCMessage {
    Ping,
    Message(MPCRecord),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MPCRecord {
    pub(crate) key: String,
    pub(crate) value: String,
    pub(crate) timeout_sec: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MPCResponse {
    Pong,
    Response(MPCResult),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MPCResult {
    Success,
    Error,
}

impl ProtocolName for MPCProtocol {
    fn protocol_name(&self) -> &[u8] {
        b"/tss/keygen"
    }
}

#[async_trait]
impl RequestResponseCodec for MPCCodec {
    type Protocol = MPCProtocol;
    type Request = MPCMessage;
    type Response = MPCResponse;

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
                    let request = proto::Message::decode(io::Cursor::new(bytes))?;
                    proto_msg_to_req(request)
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
                    let response = proto::Message::decode(io::Cursor::new(bytes))?;
                    proto_msg_to_res(response)
                }
                Err(e) => Err(io::Error::new(io::ErrorKind::InvalidData, e)),
            })
            .await
    }

    async fn write_request<T>(
        &mut self,
        _: &MPCProtocol,
        io: &mut T,
        req: MPCMessage,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        let proto_struct = req_to_proto_msg(req);
        let mut buf = Vec::with_capacity(proto_struct.encoded_len());
        proto_struct
            .encode(&mut buf)
            .expect("Vec<u8> provides capacity as needed");
        write_length_prefixed(io, buf).await
    }

    async fn write_response<T>(
        &mut self,
        _: &MPCProtocol,
        io: &mut T,
        res: MPCResponse,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        let proto_struct = res_to_proto_msg(res);
        let mut buf = Vec::with_capacity(proto_struct.encoded_len());
        proto_struct
            .encode(&mut buf)
            .expect("Vec<u8> provides capacity as needed");
        write_length_prefixed(io, buf).await
    }
}

fn proto_msg_to_req(msg: proto::Message) -> Result<MPCMessage, io::Error> {
    let msg_type = proto::message::MessageType::from_i32(msg.r#type)
        .ok_or_else(|| invalid_data(format!("unknown message type: {}", msg.r#type)))?;
    match msg_type {
        proto::message::MessageType::Ping => Ok(MPCMessage::Ping),
        proto::message::MessageType::Publish => {
            let proto_record = msg.record.unwrap_or_default();
            let record = MPCRecord {
                key: String::from_utf8(proto_record.key)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
                value: String::from_utf8(proto_record.value)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
                timeout_sec: proto_record.timeout,
            };
            Ok(MPCMessage::Message(record))
        }
    }
}

fn proto_msg_to_res(msg: proto::Message) -> Result<MPCResponse, io::Error> {
    let msg_type = proto::message::MessageType::from_i32(msg.r#type)
        .ok_or_else(|| invalid_data(format!("unknown message type: {}", msg.r#type)))?;
    match msg_type {
        proto::message::MessageType::Ping => Ok(MPCResponse::Pong),
        proto::message::MessageType::Publish => {
            match proto::message::Result::from_i32(msg.r#result)
                .ok_or_else(|| invalid_data(format!("unknown message result: {}", msg.r#result)))?
            {
                proto::message::Result::Success => {
                    Ok(MPCResponse::Response(MPCResult::Success))
                }
                proto::message::Result::Error => Ok(MPCResponse::Response(MPCResult::Error)),
            }
        }
    }
}

fn req_to_proto_msg(req: MPCMessage) -> proto::Message {
    match req {
        MPCMessage::Ping => proto::Message {
            r#type: proto::message::MessageType::Ping as i32,
            ..proto::Message::default()
        },
        MPCMessage::Message(record) => {
            let proto_record = proto::Record {
                key: record.key.into_bytes(),
                value: record.value.into_bytes(),
                timeout: record.timeout_sec,
            };
            proto::Message {
                r#type: proto::message::MessageType::Publish as i32,
                record: Some(proto_record),
                ..proto::Message::default()
            }
        }
    }
}

fn res_to_proto_msg(res: MPCResponse) -> proto::Message {
    match res {
        MPCResponse::Pong => proto::Message {
            r#type: proto::message::MessageType::Ping as i32,
            ..proto::Message::default()
        },
        MPCResponse::Response(r) => {
            let result = match r {
                MPCResult::Success => proto::message::Result::Success,
                MPCResult::Error => proto::message::Result::Error,
            };
            proto::Message {
                r#type: proto::message::MessageType::Publish as i32,
                r#result: result as i32,
                ..proto::Message::default()
            }
        }
    }
}

/// Creates an `io::Error` with `io::ErrorKind::InvalidData`.
fn invalid_data<E>(e: E) -> io::Error
where
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    io::Error::new(io::ErrorKind::InvalidData, e)
}
