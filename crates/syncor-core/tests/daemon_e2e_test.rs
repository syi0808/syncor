//! End-to-end daemon tests.
//!
//! Tests the IPC server/client communication, watcher debouncing,
//! and poller ticking — all with real async runtime, no mocking.

use std::time::Duration;
use syncor_core::daemon::server::{IpcClient, IpcRequest, IpcResponse, IpcServer};
use syncor_core::watch::poller::Poller;
use syncor_core::watch::watcher::DebouncedWatcher;
use tempfile::TempDir;
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// IPC server/client tests
// ---------------------------------------------------------------------------

/// Server receives a request and sends a response.
#[tokio::test]
async fn ipc_request_response_cycle() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("test.sock");

    let (cmd_tx, mut cmd_rx) =
        mpsc::channel::<(IpcRequest, tokio::sync::oneshot::Sender<IpcResponse>)>(10);

    let server = IpcServer::start(sock.clone(), cmd_tx).await.unwrap();

    // Handler: echo back the command name
    tokio::spawn(async move {
        while let Some((req, reply_tx)) = cmd_rx.recv().await {
            let _ = reply_tx.send(IpcResponse::ok(serde_json::json!({
                "received": req.cmd,
                "args": req.args,
            })));
        }
    });

    // Client sends "status" command
    let client = IpcClient::connect(&sock).await.unwrap();
    let resp = client
        .send(IpcRequest {
            cmd: "status".to_string(),
            args: serde_json::json!({}),
        })
        .await
        .unwrap();

    assert!(resp.ok);
    let data = resp.data.unwrap();
    assert_eq!(data["received"], "status");

    server.stop().await;
}

/// Server handles multiple sequential clients.
#[tokio::test]
async fn ipc_multiple_clients() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("multi.sock");

    let (cmd_tx, mut cmd_rx) =
        mpsc::channel::<(IpcRequest, tokio::sync::oneshot::Sender<IpcResponse>)>(10);

    let server = IpcServer::start(sock.clone(), cmd_tx).await.unwrap();

    tokio::spawn(async move {
        while let Some((req, reply_tx)) = cmd_rx.recv().await {
            let _ = reply_tx.send(IpcResponse::ok(serde_json::json!({
                "cmd": req.cmd,
            })));
        }
    });

    // Client 1
    let c1 = IpcClient::connect(&sock).await.unwrap();
    let r1 = c1
        .send(IpcRequest {
            cmd: "push".to_string(),
            args: serde_json::json!({}),
        })
        .await
        .unwrap();
    assert!(r1.ok);
    assert_eq!(r1.data.unwrap()["cmd"], "push");

    // Client 2 (new connection)
    let c2 = IpcClient::connect(&sock).await.unwrap();
    let r2 = c2
        .send(IpcRequest {
            cmd: "pull".to_string(),
            args: serde_json::json!({}),
        })
        .await
        .unwrap();
    assert!(r2.ok);
    assert_eq!(r2.data.unwrap()["cmd"], "pull");

    server.stop().await;
}

/// Server returns error response for invalid handler.
#[tokio::test]
async fn ipc_error_response() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("err.sock");

    let (cmd_tx, mut cmd_rx) =
        mpsc::channel::<(IpcRequest, tokio::sync::oneshot::Sender<IpcResponse>)>(10);

    let server = IpcServer::start(sock.clone(), cmd_tx).await.unwrap();

    tokio::spawn(async move {
        while let Some((_req, reply_tx)) = cmd_rx.recv().await {
            let _ = reply_tx.send(IpcResponse::err("unknown command"));
        }
    });

    let client = IpcClient::connect(&sock).await.unwrap();
    let resp = client
        .send(IpcRequest {
            cmd: "invalid".to_string(),
            args: serde_json::json!({}),
        })
        .await
        .unwrap();

    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap(), "unknown command");

    server.stop().await;
}

/// Client connect to non-existent socket returns DaemonNotRunning error.
#[tokio::test]
async fn ipc_connect_no_server() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("nonexistent.sock");

    let result = IpcClient::connect(&sock).await;
    assert!(
        result.is_err(),
        "should fail to connect to nonexistent socket"
    );
}

/// Server stops accepting connections after stop.
#[tokio::test]
async fn ipc_server_stops_accepting_after_stop() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("stop.sock");

    let (cmd_tx, _cmd_rx) =
        mpsc::channel::<(IpcRequest, tokio::sync::oneshot::Sender<IpcResponse>)>(10);

    let server = IpcServer::start(sock.clone(), cmd_tx).await.unwrap();
    assert!(sock.exists(), "socket should exist while server is running");

    server.stop().await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // New connections should fail or hang (server no longer accepting).
    // The socket file may still exist, but the listener is dropped.
    let result = tokio::time::timeout(Duration::from_millis(500), IpcClient::connect(&sock)).await;

    // Either connect fails, or times out — both indicate server stopped.
    match result {
        Err(_timeout) => {} // timed out = server not responding = good
        Ok(Err(_)) => {}    // connection refused = good
        Ok(Ok(client)) => {
            // Connected to stale socket — send should fail
            let send_result = client
                .send(IpcRequest {
                    cmd: "ping".to_string(),
                    args: serde_json::json!({}),
                })
                .await;
            assert!(
                send_result.is_err(),
                "should not get response from stopped server"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Watcher tests
// ---------------------------------------------------------------------------

/// Watcher detects file creation.
#[tokio::test]
async fn watcher_detects_new_file() {
    let dir = TempDir::new().unwrap();
    let (tx, mut rx) = mpsc::channel(10);

    let _watcher =
        DebouncedWatcher::start(dir.path().to_path_buf(), Duration::from_millis(100), tx).unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;
    std::fs::write(dir.path().join("new.txt"), "hello").unwrap();

    let event = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("should receive event")
        .expect("channel open");
    assert!(event.changed);
}

/// Watcher detects file modification.
#[tokio::test]
async fn watcher_detects_modification() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("existing.txt"), "v1").unwrap();

    let (tx, mut rx) = mpsc::channel(10);
    let _watcher =
        DebouncedWatcher::start(dir.path().to_path_buf(), Duration::from_millis(100), tx).unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;
    std::fs::write(dir.path().join("existing.txt"), "v2").unwrap();

    let event = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("should receive event")
        .expect("channel open");
    assert!(event.changed);
}

/// Watcher detects file deletion.
#[tokio::test]
async fn watcher_detects_deletion() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("to-delete.txt");
    std::fs::write(&path, "bye").unwrap();

    let (tx, mut rx) = mpsc::channel(10);
    let _watcher =
        DebouncedWatcher::start(dir.path().to_path_buf(), Duration::from_millis(100), tx).unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;
    std::fs::remove_file(&path).unwrap();

    let event = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("should receive event")
        .expect("channel open");
    assert!(event.changed);
}

/// Rapid changes are debounced into a single event.
#[tokio::test]
async fn watcher_debounce_coalesces() {
    let dir = TempDir::new().unwrap();
    let (tx, mut rx) = mpsc::channel(10);

    let _watcher =
        DebouncedWatcher::start(dir.path().to_path_buf(), Duration::from_millis(300), tx).unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    // 10 rapid writes
    for i in 0..10 {
        std::fs::write(dir.path().join("rapid.txt"), format!("v{}", i)).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Should get exactly 1 debounced event
    let _event = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("should receive first event")
        .expect("channel open");

    // No second event within a short window
    let second = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await;
    assert!(
        second.is_err(),
        "should NOT receive second event (debounced)"
    );
}

// ---------------------------------------------------------------------------
// Poller tests
// ---------------------------------------------------------------------------

/// Poller fires at the expected interval.
#[tokio::test]
async fn poller_fires_on_interval() {
    let (tx, mut rx) = mpsc::channel(10);
    let poller = Poller::start(Duration::from_millis(80), tx);

    tokio::time::sleep(Duration::from_millis(300)).await;
    poller.stop();

    let mut count = 0;
    while rx.try_recv().is_ok() {
        count += 1;
    }
    assert!(count >= 2, "expected >= 2 ticks in 300ms, got {}", count);
}

/// Poller stops cleanly and no more events after stop.
#[tokio::test]
async fn poller_no_events_after_stop() {
    let (tx, mut rx) = mpsc::channel(10);
    let poller = Poller::start(Duration::from_millis(50), tx);

    tokio::time::sleep(Duration::from_millis(120)).await;
    poller.stop();

    // Drain pending events
    while rx.try_recv().is_ok() {}

    // Wait and verify no more events
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(rx.try_recv().is_err(), "no events should arrive after stop");
}

// ---------------------------------------------------------------------------
// DaemonManager lifecycle
// ---------------------------------------------------------------------------

/// PID file and cleanup behavior.
#[test]
fn daemon_manager_pid_lifecycle() {
    use syncor_core::config::SyncorPaths;
    use syncor_core::daemon::manager::DaemonManager;

    let dir = TempDir::new().unwrap();
    let paths = SyncorPaths::with_home(dir.path());
    paths.ensure_dirs().unwrap();

    // Not running initially
    assert!(!DaemonManager::is_running(&paths));

    // Simulate a stale PID file with our own PID (process is alive)
    let pid = std::process::id();
    std::fs::write(paths.pid_file(), pid.to_string()).unwrap();
    assert!(
        DaemonManager::is_running(&paths),
        "should detect running process"
    );

    // Simulate a stale PID file with invalid PID
    std::fs::write(paths.pid_file(), "999999999").unwrap();
    assert!(
        !DaemonManager::is_running(&paths),
        "should detect dead process"
    );

    // Cleanup stale removes PID and socket
    std::fs::write(paths.pid_file(), "999999999").unwrap();
    std::fs::write(paths.socket_path(), "").unwrap();
    DaemonManager::cleanup_stale(&paths);
    assert!(!paths.pid_file().exists());
    assert!(!paths.socket_path().exists());
}
