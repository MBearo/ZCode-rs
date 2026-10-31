//! 运行日志（docs/specs/rust-cli/logging.md）：全局 `tracing` 订阅者把事件格式化成 Node 形状的 JSONL 行，
//! 经有界通道交给唯一的写者任务（[`crate::log_writer`]）。只写文件，从不写终端（TUI 占着终端，`-p` 的
//! stdout 是输出）。

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rust_cli_base::TaskScope;
use rust_cli_base::js::iso_timestamp;
use rust_cli_base::redact::redact_secrets;
use rust_cli_base::units;
use rust_cli_kernel_api::clock::Clock;
use serde_json::{Map, Value};
use tokio::sync::{mpsc, oneshot};
use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::Interest;
use tracing::{Event, Level, Metadata, Subscriber};

use crate::log_writer::{DailyFiles, write_loop};

/// 通道容量：发事件的一方从不等待，满了就丢（logging.md §6）。
const CAPACITY: usize = 1024;
/// 每天的上限（logging.md §1）。
const DAILY_CAP: u64 = 32 * units::MIB as u64;
const REDACTED: &str = "[Redacted]";

pub(crate) enum Item {
    Line(String),
    /// 写者写完它之前的所有行后回复。
    Flush(oneshot::Sender<()>),
}

/// 开发构建写 debug，发布构建写 info（logging.md §3）。
pub(crate) fn default_level() -> LevelFilter {
    if cfg!(debug_assertions) {
        LevelFilter::DEBUG
    } else {
        LevelFilter::INFO
    }
}

/// 进程的运行日志：写者任务与刷新入口。释放时写者被中止，所以入口在退出前先 [`RunLog::flush`]。
pub(crate) struct RunLog {
    _scope: TaskScope,
    sender: mpsc::Sender<Item>,
}

impl RunLog {
    /// 装上全局订阅者并启动写者。已经装过（同一进程第二次调用）时只是多一个不会收到行的写者。
    pub(crate) fn start(dir: PathBuf, clock: Arc<dyn Clock>) -> Self {
        let (sender, receiver) = mpsc::channel(CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));
        let subscriber = FileSubscriber::new(
            default_level(),
            clock.clone(),
            sender.clone(),
            dropped.clone(),
        );
        let _ = tracing::subscriber::set_global_default(subscriber);
        let scope = TaskScope::root("log", |_| {});
        let files = DailyFiles::new(dir, DAILY_CAP);
        let _writer = scope
            .spawner()
            .spawn("log-writer", write_loop(receiver, files, clock, dropped));
        Self {
            _scope: scope,
            sender,
        }
    }

    /// 等写者写完已经发出的行。
    pub(crate) async fn flush(&self) {
        let (reply, flushed) = oneshot::channel();
        if self.sender.send(Item::Flush(reply)).await.is_ok() {
            let _ = flushed.await;
        }
    }
}

/// 只处理事件的订阅者：没有 span（本工程不用 span），级别过滤后格式化成一行。
pub(crate) struct FileSubscriber {
    max: LevelFilter,
    clock: Arc<dyn Clock>,
    sender: mpsc::Sender<Item>,
    dropped: Arc<AtomicU64>,
}

impl FileSubscriber {
    pub(crate) fn new(
        max: LevelFilter,
        clock: Arc<dyn Clock>,
        sender: mpsc::Sender<Item>,
        dropped: Arc<AtomicU64>,
    ) -> Self {
        Self {
            max,
            clock,
            sender,
            dropped,
        }
    }
}

impl Subscriber for FileSubscriber {
    /// 每次都问 `enabled`：测试里同时存在多个级别不同的订阅者，不能缓存成 always / never。
    fn register_callsite(&self, _metadata: &'static Metadata<'static>) -> Interest {
        Interest::sometimes()
    }

    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.is_event() && *metadata.level() <= self.max
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(self.max)
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let metadata = event.metadata();
        let mut fields = Fields::default();
        event.record(&mut fields);
        let line = fields.line(self.clock.now_ms(), *metadata.level(), metadata.target());
        if self.sender.try_send(Item::Line(line)).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

/// 一个事件的字段：保留字段放到顶层（Node 的键），其余进 `context`（logging.md §2）。
#[derive(Default)]
struct Fields {
    message: String,
    event: Option<Value>,
    trace: Option<Value>,
    session: Option<Value>,
    tool_use: Option<Value>,
    duration_ms: Option<Value>,
    status: Option<Value>,
    error: Option<Value>,
    context: Map<String, Value>,
}

/// Node `DefaultLogRedactor` 的字段名规则：`api[-_]?key|authorization|cookie|credential|password|secret|token`。
fn sensitive(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["apikey", "api_key", "api-key"]
        .iter()
        .chain(&[
            "authorization",
            "cookie",
            "credential",
            "password",
            "secret",
            "token",
        ])
        .any(|word| name.contains(word))
}

fn text(value: &str) -> Value {
    Value::String(redact_secrets(value).into_owned())
}

impl Fields {
    fn put(&mut self, field: &Field, value: Value) {
        let name = field.name();
        if sensitive(name) {
            self.context
                .insert(name.to_owned(), Value::String(REDACTED.to_owned()));
            return;
        }
        let slot = match name {
            "message" => {
                self.message = value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned);
                return;
            }
            "event" => &mut self.event,
            "trace" => &mut self.trace,
            "session" => &mut self.session,
            "tool_use" => &mut self.tool_use,
            "duration_ms" => &mut self.duration_ms,
            // Node 的 `status` 是生命周期的文字（started / completed / failed …）；数字（HTTP 状态码）进 context。
            "status" if value.is_string() => &mut self.status,
            "error" => {
                let message = value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned);
                self.error = Some(serde_json::json!({ "message": message }));
                return;
            }
            _ => {
                self.context.insert(name.to_owned(), value);
                return;
            }
        };
        *slot = Some(value);
    }

    fn line(self, now_ms: u64, level: Level, target: &str) -> String {
        let mut line = Map::new();
        line.insert("timestamp".to_owned(), Value::String(iso_timestamp(now_ms)));
        line.insert(
            "level".to_owned(),
            Value::String(level_name(level).to_owned()),
        );
        if let Some(event) = self.event {
            line.insert("event".to_owned(), event);
        }
        line.insert("module".to_owned(), Value::String(target.to_owned()));
        line.insert("message".to_owned(), Value::String(self.message));
        let reserved = [
            ("traceId", self.trace),
            ("sessionId", self.session),
            ("toolCallId", self.tool_use),
            ("durationMs", self.duration_ms),
            ("status", self.status),
        ];
        for (key, value) in reserved {
            if let Some(value) = value {
                line.insert(key.to_owned(), value);
            }
        }
        if !self.context.is_empty() {
            line.insert("context".to_owned(), Value::Object(self.context));
        }
        if let Some(error) = self.error {
            line.insert("error".to_owned(), error);
        }
        Value::Object(line).to_string()
    }
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.put(field, text(&format!("{value:?}")));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field, text(value));
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.put(field, text(&value.to_string()));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, Value::from(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, Value::from(value));
    }

    /// `as_millis()` 一类的 u128：放得进 u64 时按数字写。
    fn record_u128(&mut self, field: &Field, value: u128) {
        match u64::try_from(value) {
            Ok(value) => self.put(field, Value::from(value)),
            Err(_) => self.put(field, Value::String(value.to_string())),
        }
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.put(field, Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, Value::Bool(value));
    }
}

fn level_name(level: Level) -> &'static str {
    match level {
        Level::ERROR => "error",
        Level::WARN => "warn",
        Level::INFO => "info",
        Level::DEBUG => "debug",
        Level::TRACE => "trace",
    }
}

/// 写者自己的提示行（`log.capped`、`log.dropped`），与事件同一个形状，级别 warn。
pub(crate) fn notice(
    now_ms: u64,
    event: &str,
    message: &str,
    context: Map<String, Value>,
) -> String {
    let fields = Fields {
        message: message.to_owned(),
        event: Some(Value::String(event.to_owned())),
        context,
        ..Fields::default()
    };
    fields.line(now_ms, Level::WARN, module_path!())
}

#[cfg(test)]
#[path = "log_tests.rs"]
mod tests;
