use syncor_core::daemon::server::{IpcRequest, IpcResponse, IpcServer, IpcClient};
use tempfile::TempDir;
use tokio::sync::mpsc;

#[tokio::test]
async fn ipc_roundtrip() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("test.sock");

    let (cmd_tx, mut cmd_rx) = mpsc::channel::<(IpcRequest, tokio::sync::oneshot::Sender<IpcResponse>)>(10);

    let server = IpcServer::start(sock.clone(), cmd_tx).await.unwrap();

    tokio::spawn(async move {
        while let Some((req, reply_tx)) = cmd_rx.recv().await {
            reply_tx.send(IpcResponse::ok(serde_json::json!({
                "echo": req.cmd
            }))).unwrap();
        }
    });

    let client = IpcClient::connect(&sock).await.unwrap();
    let resp = client.send(IpcRequest {
        cmd: "status".to_string(),
        args: serde_json::json!({}),
    }).await.unwrap();

    assert!(resp.ok);
    assert_eq!(resp.data.unwrap()["echo"], "status");

    server.stop().await;
}
