use async_std::task;
use curv::elliptic::curves::{Point, Secp256k1};
use futures::channel::oneshot;

use futures::future::TryFutureExt;

use mpc_p2p::RoomId;
use mpc_rpc::{RpcError, RpcErrorCode, RpcFuture, RpcResult};

use serde::de::DeserializeOwned;

use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2020::party_i::SignatureRecid;
use std::fmt::{Debug, Display};
use std::future::Future;
use std::io::{BufWriter, Write};
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};

pub struct RpcApi {
    rt_service: mpc_runtime::RuntimeService,
}

impl RpcApi {
    pub fn new(rt_service: mpc_runtime::RuntimeService) -> Self {
        Self { rt_service }
    }
}

impl mpc_rpc::JsonRPCHandler for RpcApi {
    fn keygen(&self, room: String, n: u16, t: u16) -> RpcFuture<RpcResult<Point<Secp256k1>>> {
        let mut rt_service = self.rt_service.clone();

        let (tx, rx) = oneshot::channel();

        let mut io = BufWriter::new(vec![]);
        let mut buffer = unsigned_varint::encode::u16_buffer();
        let _ = io.write_all(unsigned_varint::encode::u16(t, &mut buffer));

        task::spawn(async move {
            rt_service
                .request_computation(RoomId::from(room), n, 0, io.buffer().to_vec(), tx)
                .await;
        });

        AsyncResult::new_boxed(rx)
    }

    fn sign(&self, room: String, t: u16, msg: Vec<u8>) -> RpcFuture<RpcResult<SignatureRecid>> {
        let mut rt_service = self.rt_service.clone();

        let (tx, rx) = oneshot::channel();
        task::spawn(async move {
            rt_service
                .request_computation(RoomId::from(room), t, 1, msg, tx)
                .await;
        });

        AsyncResult::new_boxed(rx)
    }
}

#[derive(Debug)]
struct AsyncResult<T, E> {
    rx: oneshot::Receiver<Result<Vec<u8>, E>>,
    v: PhantomData<T>,
}

impl<T: DeserializeOwned + Unpin, E: Display> Future for AsyncResult<T, E> {
    type Output = RpcResult<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        return match self.rx.try_recv() {
            Ok(Some(Ok(value))) => Poll::Ready(serde_ipld_dagcbor::from_slice(&*value).map_err(
                |e| RpcError {
                    code: RpcErrorCode::InternalError,
                    message: format!(
                        "computation finished successfully but resulted an unexpected output: {e}"
                    ),
                    data: None,
                },
            )),
            Ok(Some(Err(e))) => Poll::Ready(Err(RpcError {
                code: RpcErrorCode::InternalError,
                message: format!("computation terminated with err: {e}"),
                data: None,
            })),
            Ok(None) => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(RpcError {
                code: RpcErrorCode::InternalError,
                message: format!("computation terminated with err: {e}"),
                data: None,
            })),
        };
    }
}

impl<T, E> AsyncResult<T, E> {
    pub fn new_boxed(rx: oneshot::Receiver<Result<Vec<u8>, E>>) -> Pin<Box<Self>> {
        Box::pin(Self {
            rx,
            v: Default::default(),
        })
    }
}
