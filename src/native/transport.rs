//! Bounded line transport; no subprocess, interpreter or platform shell is used.
use super::*;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt};

const MAX_LINE: usize = 1024 * 1024;
const PENDING: usize = 16;

async fn line(reader: &mut (impl AsyncBufRead + Unpin)) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return Ok(if line.is_empty() { None } else { Some(line) });
        }
        let end = chunk
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|at| at + 1);
        let length = end.unwrap_or(chunk.len());
        if line.len() + length > MAX_LINE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Protocol line exceeds 1 MiB",
            ));
        }
        line.extend_from_slice(&chunk[..length]);
        reader.consume(length);
        if end.is_some() {
            return Ok(Some(line));
        }
    }
}

pub(crate) async fn run() -> anyhow::Result<()> {
    let adapter = Arc::new(Adapter::new(Endpoints::new(None, None, None)));
    let (send, mut responses) = tokio::sync::mpsc::channel::<Value>(PENDING);
    let writer = tokio::spawn(async move {
        let mut output = tokio::io::stdout();
        while let Some(response) = responses.recv().await {
            let mut raw = serde_json::to_vec(&response)?;
            if raw.len() > MAX_LINE {
                return Err(anyhow::anyhow!("Protocol response exceeds 1 MiB"));
            }
            raw.push(b'\n');
            output.write_all(&raw).await?;
            output.flush().await?;
        }
        Ok::<(), anyhow::Error>(())
    });
    let pending = Arc::new(tokio::sync::Semaphore::new(PENDING));
    let workers = Arc::new(tokio::sync::Semaphore::new(4));
    let mut tasks = tokio::task::JoinSet::new();
    let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
    while let Some(raw) = line(&mut reader).await? {
        while tasks.try_join_next().is_some() {}
        let value = serde_json::from_slice::<Value>(&raw);
        let value = match value {
            Ok(value) => value,
            Err(_) => {
                send.send(json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Invalid JSON"}})).await?;
                continue;
            }
        };
        let id = value.get("id").cloned();
        let method = value["method"].as_str().unwrap_or_default().to_owned();
        if value["jsonrpc"] != "2.0" || method.is_empty() {
            send.send(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32600,"message":"Invalid JSON-RPC request"}})).await?;
            continue;
        }
        let Some(id) = id else { continue };
        if method == "shutdown" {
            send.send(json!({"jsonrpc":"2.0","id":id,"result":null}))
                .await?;
            break;
        }
        let params = value.get("params").cloned().unwrap_or_else(|| json!({}));
        let permit = match pending.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                send.send(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32004,"message":"Too many pending requests"}})).await?;
                continue;
            }
        };
        let reply = send.clone();
        let adapter = adapter.clone();
        let workers = workers.clone();
        let answer = async move {
            let _permit = permit;
            let _worker = workers.acquire_owned().await;
            let timeout = if matches!(
                method.as_str(),
                "track.resolve" | "album.get" | "playlist.get" | "page.get" | "library.list"
            ) {
                18
            } else {
                12
            };
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(timeout),
                adapter.handle(&method, params),
            )
            .await
            .unwrap_or_else(|_| Err(RpcError::new(-32005, "Request timed out")));
            let response = match result {
                Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
                Err(error) => {
                    let error = adapter.safe_error(error);
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":error.code,"message":error.message}})
                }
            };
            let _ = reply.send(response).await;
        };
        // Initialization must finish before any later method is dispatched.
        if value["method"] == "initialize" {
            answer.await;
        } else {
            tasks.spawn(answer);
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    drop(send);
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), writer).await;
    // The blocking stdin helper must not keep the runtime alive after the pipe closes.
    std::process::exit(0)
}
