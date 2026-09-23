//! Output sinks. Lines are serialized once by the App Server; the stdout sink
//! never blocks the caller and exposes its unwritten backlog so delivery can
//! degrade to snapshot recovery instead of buffering without bound.
use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    io::Write,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc as std_mpsc,
    },
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub enum Sink {
    Stdout {
        tx: std_mpsc::Sender<Vec<String>>,
        backlog: Arc<AtomicUsize>,
    },
    /// In-process consumer (tests, embedding) receiving parsed values per batch.
    Values(mpsc::Sender<Vec<Value>>),
}

impl Sink {
    /// Start the stdout writer thread. Each batch is written then flushed once.
    pub fn stdout(cancel: CancellationToken) -> (Self, std::thread::JoinHandle<()>) {
        let (tx, rx) = std_mpsc::channel::<Vec<String>>();
        let backlog = Arc::new(AtomicUsize::new(0));
        let pending = backlog.clone();
        let writer = std::thread::spawn(move || {
            let stdout = std::io::stdout();
            let mut out = std::io::BufWriter::with_capacity(64 * 1024, stdout.lock());
            while let Ok(batch) = rx.recv() {
                let bytes = batch.iter().map(|line| line.len() + 1).sum::<usize>();
                let written = (|| -> std::io::Result<()> {
                    for line in &batch {
                        out.write_all(line.as_bytes())?;
                        out.write_all(b"\n")?;
                    }
                    out.flush()
                })();
                pending.fetch_sub(bytes, Ordering::Relaxed);
                if written.is_err() {
                    // stdout 断管：协议通道已不可用，由取消收口整个 runtime。
                    cancel.cancel();
                    return;
                }
            }
        });
        (Self::Stdout { tx, backlog }, writer)
    }

    pub async fn send(&mut self, lines: Vec<String>) -> Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        match self {
            Self::Stdout { tx, backlog } => {
                let bytes = lines.iter().map(|line| line.len() + 1).sum::<usize>();
                backlog.fetch_add(bytes, Ordering::Relaxed);
                tx.send(lines).ok().context("Protocol writer stopped")
            }
            Self::Values(tx) => {
                let values = lines
                    .iter()
                    .map(|line| serde_json::from_str(line))
                    .collect::<Result<Vec<Value>, _>>()?;
                tx.send(values).await.ok().context("Protocol output closed")
            }
        }
    }

    pub async fn send_values(&mut self, values: Vec<Value>) -> Result<()> {
        let lines = values
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()?;
        self.send(lines).await
    }

    /// Bytes queued but not yet written to the transport.
    pub fn backlog(&self) -> usize {
        match self {
            Self::Stdout { backlog, .. } => backlog.load(Ordering::Relaxed),
            Self::Values(_) => 0,
        }
    }
}
