use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub struct PollEvent;

pub struct Poller {
    handle: JoinHandle<()>,
    cancel: tokio::sync::watch::Sender<bool>,
}

impl Poller {
    pub fn start(interval: Duration, tx: mpsc::Sender<PollEvent>) -> Self {
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);

        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // consume the immediate first tick so interval starts fresh
            ticker.tick().await;

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        if tx.send(PollEvent).await.is_err() {
                            break;
                        }
                    }
                    _ = cancel_rx.changed() => {
                        if *cancel_rx.borrow() {
                            break;
                        }
                    }
                }
            }
        });

        Self {
            handle,
            cancel: cancel_tx,
        }
    }

    pub fn stop(self) {
        let _ = self.cancel.send(true);
        self.handle.abort();
    }
}
