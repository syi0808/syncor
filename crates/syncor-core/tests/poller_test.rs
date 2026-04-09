use syncor_core::watch::poller::Poller;
use std::time::Duration;
use tokio::sync::mpsc;

#[tokio::test]
async fn poller_ticks_at_interval() {
    let (tx, mut rx) = mpsc::channel(10);
    let poller = Poller::start(Duration::from_millis(100), tx);
    tokio::time::sleep(Duration::from_millis(350)).await;
    poller.stop();

    let mut count = 0;
    while rx.try_recv().is_ok() {
        count += 1;
    }
    assert!(count >= 2, "expected >= 2 ticks, got {}", count);
}

#[tokio::test]
async fn poller_stops_when_requested() {
    let (tx, mut rx) = mpsc::channel(10);
    let poller = Poller::start(Duration::from_millis(50), tx);
    tokio::time::sleep(Duration::from_millis(100)).await;
    poller.stop();
    tokio::time::sleep(Duration::from_millis(200)).await;

    while rx.try_recv().is_ok() {}

    let result = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await;
    assert!(result.is_err() || result.unwrap().is_none());
}
