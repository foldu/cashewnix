use std::time::Duration;

use tokio::sync::mpsc;

pub struct DynamicTimer {
    timeout_tx: mpsc::Sender<Duration>,
}

impl DynamicTimer {
    pub fn new() -> (Self, mpsc::Receiver<()>) {
        let (timeout_tx, mut timeout_rx) = mpsc::channel(1);
        let (uuh_tx, uuh_rx) = mpsc::channel(1);

        tokio::spawn(async move {
            loop {
                let Some(timeout) = timeout_rx.recv().await else {
                    return;
                };

                tokio::time::sleep(timeout).await;
                if uuh_tx.send(()).await.is_err() {
                    return;
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
