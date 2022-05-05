use async_std::task;
use curv::elliptic::curves::{Point, Secp256k1};
use futures::future::{FutureExt, TryFutureExt};
use futures::StreamExt;
use futures_util::SinkExt;
use mpc_rpc::{RpcError, RpcErrorCode, RpcFuture, RpcResult};
use mpc_tss::DKG;
use std::fmt::{Debug, Display};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::oneshot;
use tokio::sync::oneshot::error::TryRecvError;

enum Call {
    Keygen(u16, oneshot::Sender<RpcResult<Point<Secp256k1>>>),
    Sign(Vec<u8>, oneshot::Sender<RpcResult<()>>),
}

pub struct RpcApi {
    rt_service: mpc_runtime::RuntimeService,
}

impl RpcApi {
    pub fn new(rt_service: mpc_runtime::RuntimeService) -> Self {
        Self { rt_service }
    }
}

#[derive(Debug)]
struct AsyncResult<T, E> {
    rx: oneshot::Receiver<Result<T, E>>,
}

impl<T, E: Display> Future for AsyncResult<T, E> {
    type Output = RpcResult<T>;

    fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            match self.rx.try_recv() {
                Ok(value) => {
                    return Poll::Ready(value.map_err(|e| RpcError {
                        code: RpcErrorCode::InternalError,
                        message: format!("keygen computation terminated with err: {e}"),
                        data: None,
                    }));
                }
                Err(e) => match e {
                    TryRecvError::Empty => {}
                    TryRecvError::Closed => {
                        return Poll::Ready(Err(RpcError {
                            code: RpcErrorCode::InternalError,
                            message: e.to_string(),
                            data: None,
                        }))
                    }
                },
            };
        }
    }
}

impl mpc_rpc::JsonRPCHandler for RpcApi {
    fn keygen(&self, n: u16, t: u16) -> RpcFuture<RpcResult<Point<Secp256k1>>> {
        let (agent, rx) = DKG::new(t, "data/player_{}/key.share".to_string());

        let mut rt_service = self.rt_service.clone();

        task::spawn(async move {
            rt_service
                .join_computation(n, mpc_runtime::ProtocolAgent::Keygen(Box::new(agent)))
                .await;
        });

        AsyncResult { rx }.boxed()
    }

    fn sign(&self, _msg: Vec<u8>) -> RpcFuture<RpcResult<()>> {
        let (tx, rx) = oneshot::channel();

        tx.send(Ok::<(), anyhow::Error>(()));

        // let background_sender = self.background.clone();
        // task::spawn(async move {
        //     background_sender.send(Call::Sign(msg, tx)).await;
        // });

        AsyncResult { rx }.boxed()
    }
}
