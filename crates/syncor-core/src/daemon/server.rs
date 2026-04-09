use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};

/// A request sent from an IPC client to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcRequest {
    pub cmd: String,
    pub args: Value,
}

/// A response sent from the daemon back to the IPC client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub ok: bool,
    pub data: Option<Value>,
    pub error: Option<String>,
}

impl IpcResponse {
    pub fn ok(data: Value) -> Self {
        IpcResponse {
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        IpcResponse {
            ok: false,
            data: None,
            error: Some(msg.into()),
        }
    }
}

/// Type alias for the channel that sends IPC commands to the daemon command loop.
pub type CommandSender = mpsc::Sender<(IpcRequest, oneshot::Sender<IpcResponse>)>;

/// Handle to the IPC server. Drop or call `stop()` to shut it down.
pub struct IpcServer {
    shutdown_tx: oneshot::Sender<()>,
}

impl IpcServer {
    /// Start the IPC server, binding to `sock_path` and forwarding parsed requests
    /// through `cmd_sender`.
    pub async fn start(
        sock_path: impl AsRef<Path>,
        cmd_sender: CommandSender,
    ) -> std::io::Result<Self> {
        let sock_path = sock_path.as_ref();

        // Remove stale socket if present.
        if sock_path.exists() {
            std::fs::remove_file(sock_path)?;
        }

        let listener = UnixListener::bind(sock_path)?;
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    result = listener.accept() => {
                        match result {
                            Ok((stream, _addr)) => {
                                let sender = cmd_sender.clone();
                                tokio::spawn(handle_connection(stream, sender));
                            }
                            Err(e) => {
                                tracing::error!("IPC accept error: {e}");
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(IpcServer { shutdown_tx })
    }

    /// Signal the server to stop accepting new connections.
    pub async fn stop(self) {
        let _ = self.shutdown_tx.send(());
    }
}

async fn handle_connection(stream: UnixStream, cmd_sender: CommandSender) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break, // EOF
            Ok(_) => {
                let trimmed = line.trim_end();
                if trimmed.is_empty() {
                    continue;
                }
                let req: IpcRequest = match serde_json::from_str(trimmed) {
                    Ok(r) => r,
                    Err(e) => {
                        let resp = IpcResponse::err(format!("parse error: {e}"));
                        let _ = send_response(&mut write_half, &resp).await;
                        continue;
                    }
                };

                let (reply_tx, reply_rx) = oneshot::channel::<IpcResponse>();
                if cmd_sender.send((req, reply_tx)).await.is_err() {
                    let resp = IpcResponse::err("daemon unavailable");
                    let _ = send_response(&mut write_half, &resp).await;
                    break;
                }

                match reply_rx.await {
                    Ok(resp) => {
                        if send_response(&mut write_half, &resp).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => {
                        let resp = IpcResponse::err("no reply from daemon");
                        let _ = send_response(&mut write_half, &resp).await;
                        break;
                    }
                }
            }
            Err(e) => {
                tracing::error!("IPC read error: {e}");
                break;
            }
        }
    }
}

async fn send_response(
    write_half: &mut tokio::net::unix::OwnedWriteHalf,
    resp: &IpcResponse,
) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(resp).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::Other, e)
    })?;
    bytes.push(b'\n');
    write_half.write_all(&bytes).await
}

/// IPC client that connects to a daemon over a Unix domain socket.
pub struct IpcClient {
    stream: UnixStream,
}

impl IpcClient {
    /// Connect to the Unix domain socket at `sock_path`.
    pub async fn connect(sock_path: impl AsRef<Path>) -> std::io::Result<Self> {
        let stream = UnixStream::connect(sock_path).await?;
        Ok(IpcClient { stream })
    }

    /// Send a request and wait for the response.
    pub async fn send(self, request: IpcRequest) -> std::io::Result<IpcResponse> {
        // We consume `self` to keep borrowing simple; a production client would
        // keep the stream around for multiple requests.
        let mut stream = self.stream;

        let mut payload = serde_json::to_vec(&request).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::Other, e)
        })?;
        payload.push(b'\n');
        stream.write_all(&payload).await?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await?;

        let resp: IpcResponse = serde_json::from_str(line.trim_end()).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e)
        })?;
        Ok(resp)
    }
}
