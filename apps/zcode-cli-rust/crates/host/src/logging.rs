//! JSONL file logging compatible with the Node CLI log format.
//!
//! Runtime code logs through `tracing` macros with target prefix `zcode`. This
//! subscriber formats events like Node `adapters/logging/serialize.ts`, redacts
//! credential-like keys, and hands lines to a bounded queue drained by one
//! writer thread. When the queue is full the line is dropped and counted; a
//! logging stall never blocks the runtime. stdout is never written.
//!
//! The global subscriber keeps a queue sender for the process lifetime, so the
//! writer thread never observes a closed channel. Shutdown therefore flushes
//! with an explicit marker and a bounded wait instead of joining the thread.
use serde_json::{Map, Value, json};
use std::{
    fmt::Write as _,
    io::Write as _,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{SyncSender, TrySendError, sync_channel},
    },
    time::Duration,
};
use tracing::{
    Event, Level, Metadata, Subscriber,
    field::{Field, Visit},
    span::{Attributes, Id, Record},
};

const QUEUE_LINES: usize = 4096;
/// Upper bound for flushing queued lines at shutdown.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(1);

enum Message {
    Line(String),
    /// Everything queued before this marker has been written when the ack fires.
    Flush(SyncSender<()>),
}
/// Node `serialize.ts` redaction rule for object keys.
const REDACTED_KEYS: &[&str] = &[
    "apikey",
    "api-key",
    "api_key",
    "authorization",
    "cookie",
    "credential",
    "password",
    "secret",
    "token",
];

pub struct LogOptions {
    pub directory: PathBuf,
    pub level: Level,
    pub console: bool,
}

impl LogOptions {
    /// `ZCODE_LOG_DIR` or `~/.zcode/cli/log`; debug in development, info otherwise.
    pub fn from_env(home: &std::path::Path) -> Self {
        let directory = std::env::var_os("ZCODE_LOG_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".zcode/cli/log"));
        let development = std::env::var("ZCODE_RUNTIME_ENV").is_ok_and(|v| v == "development");
        Self {
            directory,
            level: if development { Level::DEBUG } else { Level::INFO },
            console: std::env::var("ZCODE_LOG_CONSOLE").is_ok_and(|v| v == "1"),
        }
    }
}

/// Dropping the guard flushes queued lines (bounded by [`FLUSH_TIMEOUT`]).
pub struct LogGuard {
    tx: SyncSender<Message>,
    dropped: Arc<AtomicU64>,
}

impl LogGuard {
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for LogGuard {
    fn drop(&mut self) {
        let (ack, done) = sync_channel(1);
        // 队列满或写线程卡在 IO 时最多等待 FLUSH_TIMEOUT，日志不能阻止进程退出。
        if self.tx.try_send(Message::Flush(ack)).is_ok() {
            let _ = done.recv_timeout(FLUSH_TIMEOUT);
        }
    }
}

/// Install the process-wide JSONL subscriber. Returns `None` if one is already installed.
pub fn init(options: LogOptions) -> Option<LogGuard> {
    let (tx, rx) = sync_channel::<Message>(QUEUE_LINES);
    let dropped = Arc::new(AtomicU64::new(0));
    let subscriber = JsonLines {
        level: options.level,
        tx: tx.clone(),
        dropped: dropped.clone(),
    };
    tracing::subscriber::set_global_default(subscriber).ok()?;
    let console = options.console;
    let directory = options.directory;
    std::thread::spawn(move || {
        let mut current: Option<(String, std::fs::File)> = None;
        while let Ok(message) = rx.recv() {
            let line = match message {
                Message::Line(line) => line,
                Message::Flush(ack) => {
                    let _ = ack.send(());
                    continue;
                }
            };
            if console {
                let _ = writeln!(std::io::stderr().lock(), "{line}");
            }
            let date = chrono::Local::now().format("%Y-%m-%d").to_string();
            if current.as_ref().is_none_or(|(day, _)| *day != date) {
                let _ = std::fs::create_dir_all(&directory);
                current = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(directory.join(format!("zcode-rust-{date}.jsonl")))
                    .ok()
                    .map(|file| (date, file));
            }
            if let Some((_, file)) = &mut current {
                // 与 Node 一致：写失败只丢弃该行，不影响 runtime。
                let _ = file.write_all(format!("{line}\n").as_bytes());
            }
        }
    });
    Some(LogGuard { tx, dropped })
}

struct JsonLines {
    level: Level,
    tx: SyncSender<Message>,
    dropped: Arc<AtomicU64>,
}

fn redacted(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    REDACTED_KEYS.iter().any(|needle| key.contains(needle))
}

/// Promoted to top-level line fields (Node reserved keys); everything else goes to `context`.
fn reserved(key: &str) -> Option<&'static str> {
    Some(match key {
        "event" => "event",
        "module" => "module",
        "trace_id" | "traceId" => "traceId",
        "session_id" | "sessionId" => "sessionId",
        "turn_id" | "turnId" => "turnId",
        "tool_call_id" | "toolCallId" => "toolCallId",
        "duration_ms" | "durationMs" => "durationMs",
        "status" => "status",
        "error" => "error",
        _ => return None,
    })
}

#[derive(Default)]
struct Fields {
    message: String,
    top: Map<String, Value>,
    context: Map<String, Value>,
}

impl Fields {
    fn put(&mut self, field: &Field, value: Value) {
        let name = field.name();
        if name == "message" {
            self.message = value.as_str().map_or_else(|| value.to_string(), str::to_owned);
        } else if let Some(key) = reserved(name) {
            self.top.insert(key.into(), value);
        } else if redacted(name) {
            self.context.insert(name.into(), "[Redacted]".into());
        } else {
            self.context.insert(name.into(), value);
        }
    }
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let mut text = String::new();
        let _ = write!(text, "{value:?}");
        self.put(field, Value::String(text));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field, value.into());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, value.into());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, value.into());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, value.into());
    }
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.put(field, json!({"message": value.to_string()}));
    }
}

fn level_name(level: &Level) -> &'static str {
    match *level {
        Level::ERROR => "error",
        Level::WARN => "warn",
        Level::INFO => "info",
        _ => "debug",
    }
}

fn format_line(metadata: &Metadata<'_>, fields: Fields) -> String {
    let mut line = Map::new();
    line.insert(
        "timestamp".into(),
        chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string()
            .into(),
    );
    line.insert("level".into(), level_name(metadata.level()).into());
    line.insert("module".into(), metadata.target().into());
    line.insert("message".into(), fields.message.into());
    line.extend(fields.top);
    if !fields.context.is_empty() {
        line.insert("context".into(), Value::Object(fields.context));
    }
    Value::Object(line).to_string()
}

impl Subscriber for JsonLines {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        // 只记录本仓库的事件；依赖库（hyper、rmcp 等）的内部事件不进入产品日志。
        *metadata.level() <= self.level && metadata.target().starts_with("zcode")
    }
    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(self.level.into())
    }
    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let line = format_line(event.metadata(), fields);
        if let Err(TrySendError::Full(_)) = self.tx.try_send(Message::Line(line)) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_follow_node_fields_and_redact_credentials() {
        let (tx, rx) = sync_channel(8);
        let subscriber = JsonLines {
            level: Level::INFO,
            tx,
            dropped: Arc::new(AtomicU64::new(0)),
        };
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: "zcode::test",
                event = "session.started",
                session_id = "s1",
                api_key = "secret-value",
                count = 3,
                "Session started"
            );
            tracing::debug!(target: "zcode::test", "hidden at info level");
            tracing::info!(target: "hyper::proto", "dependency noise");
        });
        let Ok(Message::Line(line)) = rx.recv() else {
            panic!("expected a log line")
        };
        let line: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(line["level"], "info");
        assert_eq!(line["event"], "session.started");
        assert_eq!(line["sessionId"], "s1");
        assert_eq!(line["message"], "Session started");
        assert_eq!(line["context"]["api_key"], "[Redacted]");
        assert_eq!(line["context"]["count"], 3);
        assert!(rx.try_recv().is_err(), "debug and dependency events are filtered");
    }

    #[test]
    fn full_queue_drops_instead_of_blocking() {
        let (tx, _rx) = sync_channel(1);
        let dropped = Arc::new(AtomicU64::new(0));
        let subscriber = JsonLines {
            level: Level::INFO,
            tx,
            dropped: dropped.clone(),
        };
        tracing::subscriber::with_default(subscriber, || {
            for _ in 0..3 {
                tracing::info!(target: "zcode::test", "line");
            }
        });
        assert_eq!(dropped.load(Ordering::Relaxed), 2);
    }
}
