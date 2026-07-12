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
        let (tick_tx, tick_rx) = mpsc::channel(1);

        tokio::spawn(async move {
            let mut next_timeout: Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
                { Box::pin(std::future::pending()) };

            loop {
                tokio::select! {
                    Some(timeout) = timeout_rx.recv() => {
                        tracing::trace!("new timeout = {:?}", timeout);
                        next_timeout = Box::pin(tokio::time::sleep(timeout))
                    },
                    _ = next_timeout => {
                        tracing::trace!("timer expired");
                        if let Err(e) = tick_tx.try_send(()) {
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
        (Self { timeout_tx }, tick_rx)
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::sync::mpsc;
    use tokio::time::timeout;

    use super::*;

    #[tokio::test]
    async fn timer_fires_after_set_timeout() {
        let (timer, mut ticks) = DynamicTimer::new();
        timer.set_timeout(Duration::from_millis(10)).await;

        let result = timeout(Duration::from_secs(1), ticks.recv()).await;
        assert!(result.is_ok(), "tick should fire after set_timeout");
        assert!(result.unwrap().is_some(), "tick should be Some(())");
    }

    #[tokio::test]
    async fn no_tick_without_set_timeout() {
        let (_timer, mut ticks) = DynamicTimer::new();

        let result = timeout(Duration::from_millis(50), ticks.recv()).await;
        assert!(result.is_err(), "no tick should fire without set_timeout");
    }

    #[tokio::test]
    async fn restart_on_second_set_timeout() {
        let (timer, mut ticks) = DynamicTimer::new();
        timer.set_timeout(Duration::from_secs(10)).await;
        timer.set_timeout(Duration::from_millis(10)).await;

        // Should fire after the shorter (10ms) timeout, not the 10s one.
        let result = timeout(Duration::from_secs(1), ticks.recv()).await;
        assert!(
            result.is_ok(),
            "tick should fire after the restarted (shorter) timeout"
        );
    }

    #[tokio::test]
    async fn rapid_set_timeout_no_panic() {
        let (timer, mut ticks) = DynamicTimer::new();

        // Fire many set_timeout calls rapidly; none should panic.
        for _ in 0..100 {
            timer.set_timeout(Duration::from_millis(1)).await;
        }

        // A tick should still eventually fire (after the last timeout).
        let result = timeout(Duration::from_secs(1), ticks.recv()).await;
        assert!(result.is_ok(), "tick should fire even after rapid restarts");
    }

    #[tokio::test]
    async fn rearm_after_tick() {
        let (timer, mut ticks) = DynamicTimer::new();

        timer.set_timeout(Duration::from_millis(5)).await;
        ticks.recv().await;

        timer.set_timeout(Duration::from_millis(5)).await;
        let result = timeout(Duration::from_secs(1), ticks.recv()).await;
        assert!(result.is_ok(), "second tick should fire after re-arming");
    }

    #[tokio::test]
    async fn debounce_single_event() {
        let (tx, rx) = mpsc::channel(4);
        let mut debounced = debounce(rx, Duration::from_millis(10));

        tx.send(()).await.unwrap();
        let result = timeout(Duration::from_secs(1), debounced.recv()).await;
        assert!(
            result.is_ok(),
            "debounced tick should fire after quiet period"
        );
        assert!(result.unwrap().is_some());
    }

    #[tokio::test]
    async fn debounce_burst_produces_one_tick() {
        let (tx, rx) = mpsc::channel(4);
        let mut debounced = debounce(rx, Duration::from_millis(20));

        // Send a burst of events quickly.
        for _ in 0..10 {
            tx.send(()).await.unwrap();
        }

        // First tick fires.
        let result = timeout(Duration::from_secs(1), debounced.recv()).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_some());

        // No second tick should arrive (the burst was coalesced).
        let result = timeout(Duration::from_millis(100), debounced.recv()).await;
        assert!(result.is_err(), "only one tick should fire for a burst");
    }

    #[tokio::test]
    async fn debounce_resets_on_new_event() {
        let (tx, rx) = mpsc::channel(4);
        let mut debounced = debounce(rx, Duration::from_millis(100));

        tx.send(()).await.unwrap();
        // Send another event before the quiet period expires.
        tokio::time::sleep(Duration::from_millis(20)).await;
        tx.send(()).await.unwrap();

        // Tick should fire ~100ms after the *second* event, not the first.
        let start = tokio::time::Instant::now();
        let result = timeout(Duration::from_secs(1), debounced.recv()).await;
        assert!(result.is_ok());
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(80),
            "tick should fire after quiet period from last event (elapsed {elapsed:?})"
        );
    }

    #[tokio::test]
    async fn debounce_continuous_events_block_tick() {
        let (tx, rx) = mpsc::channel(4);
        let mut debounced = debounce(rx, Duration::from_millis(15));

        // Keep sending events faster than the quiet period.
        for _ in 0..8 {
            tx.send(()).await.unwrap();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        // No tick should have fired yet.
        let result = timeout(Duration::from_millis(5), debounced.recv()).await;
        assert!(result.is_err(), "no tick while events keep arriving");

        // Now stop; a tick should fire after the quiet period.
        let result = timeout(Duration::from_secs(1), debounced.recv()).await;
        assert!(result.is_ok(), "tick should fire after events stop");
    }

    #[tokio::test]
    async fn debounce_multiple_bursts() {
        let (tx, rx) = mpsc::channel(4);
        let mut debounced = debounce(rx, Duration::from_millis(10));

        // First burst.
        for _ in 0..3 {
            tx.send(()).await.unwrap();
        }
        assert!(timeout(Duration::from_secs(1), debounced.recv())
            .await
            .is_ok());

        // Second burst after a gap.
        tokio::time::sleep(Duration::from_millis(30)).await;
        for _ in 0..3 {
            tx.send(()).await.unwrap();
        }
        let result = timeout(Duration::from_secs(1), debounced.recv()).await;
        assert!(result.is_ok(), "second burst should also produce a tick");
    }

    #[tokio::test]
    async fn debounce_sender_dropped_stops_output() {
        let (tx, rx) = mpsc::channel(4);
        let mut debounced = debounce(rx, Duration::from_millis(10));

        tx.send(()).await.unwrap();
        drop(tx);

        // The task should eventually exit; recv returns None.
        // Give it time to process the event, debounce, and shut down.
        let result = timeout(Duration::from_secs(1), debounced.recv()).await;
        match result {
            Ok(Some(())) => {
                // The tick for the event we sent fires, then the task exits.
                let next = timeout(Duration::from_secs(1), debounced.recv()).await;
                assert!(next.is_ok());
                assert!(
                    next.unwrap().is_none(),
                    "channel should close after sender dropped"
                );
            }
            Ok(None) => {} // Task already exited before tick fired (race).
            Err(_) => panic!("recv should not hang after sender dropped"),
        }
    }
}
