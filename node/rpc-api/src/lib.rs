use async_std::task;
use curv::elliptic::curves::{Point, Secp256k1};
use futures::channel::oneshot;

use futures::future::TryFutureExt;

use mpc_p2p::RoomId;
use mpc_rpc::{RpcError, RpcErrorCode, RpcFuture, RpcResult};

use serde::de::DeserializeOwned;

use std::fmt::{Debug, Display};
use std::future::Future;
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
    fn keygen(&self, room: String, n: u16, _t: u16) -> RpcFuture<RpcResult<Point<Secp256k1>>> {
        let mut rt_service = self.rt_service.clone();

        let (tx, rx) = futures::channel::oneshot::channel();

        task::spawn(async move {
            rt_service
                .join_computation(RoomId::from(room), n, 0, tx)
                .await;
        });

        AsyncResult::new_boxed(rx)
    }

    // fn sign(&self, _room: String, _msg: Vec<u8>) -> RpcFuture<RpcResult<()>> {
    //     let (tx, rx) = oneshot::channel();
    //
    //     tx.send(Ok::<(), anyhow::Error>(vec![]));
    //
    //     // let background_sender = self.background.clone();
    //     // task::spawn(async move {
    //     //     background_sender.send(Call::Sign(msg, tx)).await;
    //     // });
    //
    //     AsyncResult::new_boxed(rx)
    // }
}

#[derive(Debug)]
struct AsyncResult<T, E> {
    rx: oneshot::Receiver<Result<Vec<u8>, E>>,
    v: PhantomData<T>,
}

impl<T: DeserializeOwned + Unpin, E: Display> Future for AsyncResult<T, E> {
    type Output = RpcResult<T>;

    fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            match self.rx.try_recv() {
                Ok(Some(Ok(value))) => {
                    return Poll::Ready(serde_ipld_dagcbor::from_slice(&*value).map_err(|e| RpcError {
                        code: RpcErrorCode::InternalError,
                        message: format!("computation finished successfully but resulted an unexpected output: {e}"),
                        data: None,
                    }))
                }
                Ok(Some(Err(e))) => {
                    return Poll::Ready(Err(RpcError {
                        code: RpcErrorCode::InternalError,
                        message: format!("keygen computation terminated with err: {e}"),
                        data: None,
                    }))
                }
                Ok(None) => {},
                Err(e) => {
                    return Poll::Ready(Err(RpcError {
                        code: RpcErrorCode::InternalError,
                        message: format!("keygen computation terminated with err: {e}"),
                        data: None,
                    }))
                }
            };
        }
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
