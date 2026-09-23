use super::Sink;
use crate::domain::{MAX_REQUEST_BYTES, protocol::Request};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::io::BufRead;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub use crate::contract::Input;

/// Start stdin framing: one NDJSON message per line, bounded by `MAX_REQUEST_BYTES`.
pub fn start(input_closed: CancellationToken) -> mpsc::Receiver<Input> {
    let (in_tx, in_rx) = mpsc::channel(64);
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut reader = stdin.lock();
        let mut line = Vec::new();
        while let Ok(available) = reader.fill_buf() {
            if available.is_empty() {
                if !line.is_empty() {
                    dispatch(&in_tx, &line);
                }
                break;
            }
            let take = available
                .iter()
                .position(|b| *b == b'\n')
                .map_or(available.len(), |i| i + 1);
            if line.len() + take > MAX_REQUEST_BYTES {
                let _ = in_tx.blocking_send(Input::TooLarge);
                break;
            }
            line.extend_from_slice(&available[..take]);
            reader.consume(take);
            if line.last() == Some(&b'\n') {
                if !dispatch(&in_tx, &line) {
                    return;
                }
                line.clear();
            }
        }
        input_closed.cancel();
        let _ = in_tx.blocking_send(Input::Eof);
    });
    in_rx
}
fn dispatch(tx: &mpsc::Sender<Input>, line: &[u8]) -> bool {
    if line.iter().all(u8::is_ascii_whitespace) {
        return true;
    }
    let value = match serde_json::from_slice::<Value>(line) {
        Ok(v) => v,
        Err(_) => return tx.blocking_send(Input::Invalid).is_ok(),
    };
    if value.get("method").is_none()
        && let Some(id) = value["id"].as_str()
        && (value.get("result").is_some() ^ value.get("error").is_some())
    {
        return tx
            .blocking_send(Input::Response {
                id: id.into(),
                result: value.get("result").cloned().unwrap_or(Value::Null),
            })
            .is_ok();
    }
    let request = {
        if value["method"] == "startup/storagePathReady" {
            serde_json::from_value(
                json!({"method":"startup/storagePathReady","params":{"reuse":value["reuse"]}}),
            )
        } else {
            serde_json::from_value::<Request>(value)
        }
    };
    tx.blocking_send(match request {
        Ok(r) if !r.method.trim().is_empty() => Input::Request(r),
        _ => Input::Invalid,
    })
    .is_ok()
}
pub async fn finish(writer: std::thread::JoinHandle<()>) -> Result<()> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while !writer.is_finished() {
        if tokio::time::Instant::now() >= deadline {
            bail!("Protocol output drain timed out");
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    writer
        .join()
        .map_err(|_| anyhow::anyhow!("Protocol writer failed"))
}

pub async fn storage_prepare(
    path: &std::path::Path,
    input: &mut mpsc::Receiver<Input>,
    output: &mut Sink,
) -> Result<()> {
    output
        .send_values(vec![
            json!({"method":"startup/storagePath","params":{"path":path}}),
        ])
        .await?;
    match tokio::time::timeout(std::time::Duration::from_secs(30), input.recv()).await? {
        Some(Input::Request(request)) if request.method == "startup/storagePathReady" => Ok(()),
        _ => bail!("Storage preparation handshake failed"),
    }
}
