use futures::future::{FutureExt, TryFutureExt};
use futures::StreamExt;
use futures_util::SinkExt;

use log::{error, info};

use mpc_rpc::{RpcError, RpcErrorCode, RpcFuture, RpcResult};

use std::fmt::Debug;
use std::fs::File;
use std::future::Future;
use std::io::Write;
use std::pin::Pin;

use anyhow::anyhow;
use async_std::task;
use curv::elliptic::curves::{Point, Secp256k1};

use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::sync::Mutex;

use tokio::sync::oneshot::error::TryRecvError;
use tokio::sync::{mpsc, oneshot};

enum Call {
    Keygen(u16, u16, oneshot::Sender<RpcResult<Point<Secp256k1>>>),
    Sign(Vec<u8>, oneshot::Sender<RpcResult<()>>),
}

pub struct RpcApi {
    background: mpsc::Sender<Call>,
}

impl RpcApi {
    pub fn new(keygen_agent: mpc_tss::DKG) -> Self {
        let (sender, mut receiver) = mpsc::channel(1);
        let keygen_agent = Arc::new(Mutex::new(Some(keygen_agent)));
        tokio::spawn(async move {
            loop {
                if let Some(call) = receiver.recv().await {
                    match call {
                        Call::Keygen(t, n, tx) => {
                            info!("running computation for 'keygen' protocol");
                            tx.send(compute_keygen(keygen_agent.clone(), t, n).await.map_err(
                                |e| RpcError {
                                    code: RpcErrorCode::InternalError,
                                    message: format!("keygen computation terminated with err: {e}"),
                                    data: None,
                                },
                            ));
                        }
                        Call::Sign(_, _) => {}
                    }
                } else {
                    error!("background channel closed unexpectedly")
                }
            }
        });

        Self { background: sender }
    }
}

#[derive(Debug)]
struct AsyncResponse<T> {
    rx: oneshot::Receiver<RpcResult<T>>,
}

impl<T> Future for AsyncResponse<T> {
    type Output = RpcResult<T>;

    fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            match self.rx.try_recv() {
                Ok(value) => {
                    return Poll::Ready(value);
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
    fn keygen(&self, t: u16, n: u16) -> RpcFuture<RpcResult<Point<Secp256k1>>> {
        let (tx, rx) = oneshot::channel();

        let background_sender = self.background.clone();
        task::spawn(async move {
            background_sender.send(Call::Keygen(t, n, tx)).await;
        });

        AsyncResponse { rx }.boxed()
    }

    fn sign(&self, msg: Vec<u8>) -> RpcFuture<RpcResult<()>> {
        let (tx, rx) = oneshot::channel();

        let background_sender = self.background.clone();
        task::spawn(async move {
            background_sender.send(Call::Sign(msg, tx)).await;
        });

        AsyncResponse { rx }.boxed()
    }
}

async fn compute_keygen(
    agent: Arc<Mutex<Option<mpc_tss::DKG>>>,
    t: u16,
    n: u16,
) -> anyhow::Result<Point<Secp256k1>> {
    let mut agent = agent.lock().await;
    let key_share = agent.take().unwrap().compute(t, n).await?;

    let mut file = File::create(format!("data/player_{}.share", key_share.i))
        .map_err(|e| anyhow!("failed to open file: {e}"))?;

    let share_bytes = serde_json::to_vec(&key_share)
        .map_err(|e| anyhow!("share serialization terminated with error: {e}"))?;

    file.write(&share_bytes)
        .map_err(|e| anyhow!("share serialization terminated with error: {e}"))?;

    Ok(key_share.y_sum_s)
}
