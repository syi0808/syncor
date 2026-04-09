use std::fs;
use std::time::Duration;
use syncor_core::watch::watcher::DebouncedWatcher;
use tempfile::TempDir;
use tokio::sync::mpsc;

#[tokio::test]
async fn watcher_detects_file_creation() {
    let dir = TempDir::new().unwrap();
    let (tx, mut rx) = mpsc::channel(10);

    let _watcher =
        DebouncedWatcher::start(dir.path().to_path_buf(), Duration::from_millis(100), tx).unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;
    fs::write(dir.path().join("test.txt"), "hello").unwrap();

    let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("timeout waiting for event")
        .expect("channel closed");
    assert!(event.changed);
}

#[tokio::test]
async fn watcher_debounces_rapid_changes() {
    let dir = TempDir::new().unwrap();
    let (tx, mut rx) = mpsc::channel(10);

    let _watcher =
        DebouncedWatcher::start(dir.path().to_path_buf(), Duration::from_millis(500), tx).unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    for i in 0..5 {
        fs::write(dir.path().join("test.txt"), format!("v{}", i)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("timeout")
        .expect("closed");
    assert!(event.changed);

    let no_event = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await;
    assert!(no_event.is_err());
}
