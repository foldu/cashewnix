use std::time::Duration;

use tokio::sync::mpsc;

use std::pin::Pin;

use tokio::sync::mpsc::error::TrySendError;

pub struct DynamicTimer {
    timeout_tx: mpsc::Sender<Duration>,
}


impl DynamicTimer {
    pub fn new() -> (Self, mpsc::Receiver<()>) {
        let (timeout_tx, mut timeout_rx) = mpsc::channel(1);
        let (uuh_tx, uuh_rx) = mpsc::channel(1);

        tokio::spawn(async move {
            let mut next_timeout: Pin<Box<dyn std::future::Future<Output = ()> + Send>> = {
                Box::pin(std::future::pending())
            };

            loop {
                tokio::select! {
                    Some(timeout) = timeout_rx.recv() => {
                        tracing::trace!("new timeout = {:?}", timeout);
                        next_timeout = Box::pin(tokio::time::sleep(timeout))
                    },
                    _ = next_timeout => {
                        tracing::trace!("timer expired");
                        if let Err(e) = uuh_tx.try_send(()) {
                            match e {
                                TrySendError::Full(_)   => {
                                    // NOTE: consider triggering a restart if this is not cleared within a certain amount of time
                                    tracing::warn!("timer expired but main task lagged or blocked")
                                },
                                TrySendError::Closed(_) => return,
                            }
                        };
                        next_timeout = Box::pin(std::future::pending())
                    },
                    else => return,
                }
            }
        });
        (Self { timeout_tx }, uuh_rx)
    }

    pub async fn set_timeout(&self, timeout: Duration) {
        self.timeout_tx.send(timeout).await.unwrap();
    }
}

pub fn debounce(mut stream: mpsc::Receiver<()>, timeout: Duration) -> mpsc::Receiver<()> {
    let (tx, rx) = mpsc::channel(1);
    tokio::spawn(async move {
        loop {
            if stream.recv().await.is_none() {
                return;
            }
            let mut timer = tokio::time::sleep(timeout);
            loop {
                tokio::select! {
                    Some(_) = stream.recv() => {
                        timer = tokio::time::sleep(timeout);
                    }
                    _ = timer => break,
                    else => return,
                }
            }
            if tx.send(()).await.is_err() {
                return;
            }
        }
    });

    rx
}
