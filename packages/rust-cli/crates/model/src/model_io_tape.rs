//! 一次 HTTP 发送的录音（docs/specs/rust-cli/model-io.md §6）。
//!
//! [`Trail`] 是一次请求的记录方与共用信息；每次发送由它开一个 [`Tape`]。成功打开的流经 [`Capture::wrap`] 包住解码器：
//! 解码前存下原始事件的 `data`，解码后看解码器产出了哪些块，认出跳过的块（不改解码器）。发送结束（或 `Tape` 被丢弃）
//! 时把记录交给 [`ModelIoSink`]，只做入队。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::model::{ModelError, ModelEvent, ModelRequest, QuerySource};
use rust_cli_kernel_api::provider::{ModelEndpoint, WireProtocol};
use rust_cli_net::sse::SseEvent;
use rust_cli_net::{HttpRequest, Method, ResponseHeaders};
use serde_json::Value;

use crate::attribution::REQUEST_ID;
use crate::model_io::{
    ModelIoRecord, ModelIoSink, Outcome, RequestFacts, ResponseBody, ResponsePart, Summary,
    path_of, request_headers, response_headers,
};
use crate::protocol::StreamDecoder;

/// 一次请求的记录方与各次发送共用的信息。
#[derive(Clone)]
pub(crate) struct Trail {
    sink: Arc<dyn ModelIoSink>,
    facts: Arc<RequestFacts>,
    clock: Arc<dyn Clock>,
}

fn route_of(protocol: WireProtocol) -> &'static str {
    match protocol {
        WireProtocol::AnthropicMessages => "messages",
        WireProtocol::OpenAiChatCompletions => "chat_completions",
        WireProtocol::OpenAiResponses => "responses",
    }
}

fn class_of(source: &QuerySource) -> &'static str {
    match source {
        QuerySource::MainThread => "main",
        QuerySource::Subagent(_) => "subagent",
        QuerySource::Compact | QuerySource::Side(_) => "side",
    }
}

impl Trail {
    /// 消息请求：在编码之前取（编码会消费请求）。
    pub(crate) fn for_request(
        (sink, clock): (Arc<dyn ModelIoSink>, Arc<dyn Clock>),
        request: &ModelRequest,
        endpoint: &ModelEndpoint,
        profile: &'static str,
        trace: Option<&str>,
    ) -> Self {
        let options = &request.options;
        let facts = RequestFacts {
            session: options.session_id.as_ref().map(|id| id.as_str().to_owned()),
            turn: options.run.as_ref().map(|run| run.as_str().to_owned()),
            trace: trace.map(str::to_owned),
            query_source: request.source.node_name(),
            class: class_of(&request.source),
            route: route_of(endpoint.protocol),
            model: endpoint.model.clone(),
            provider: request.selection.provider.clone(),
            protocol: endpoint.protocol,
            profile,
        };
        Self {
            sink,
            facts: Arc::new(facts),
            clock,
        }
    }

    /// 第 `attempt` 次发送 `http`（已合并凭据与每请求头）。
    pub(crate) fn tape(&self, attempt: u32, http: &HttpRequest) -> Tape {
        let request_id = http
            .headers
            .iter()
            .rev()
            .find(|(name, _)| name.eq_ignore_ascii_case(REQUEST_ID))
            .and_then(|(_, value)| match value {
                rust_cli_net::HeaderValue::Plain(value) => Some(value.clone()),
                rust_cli_net::HeaderValue::Secret(_) => None,
            })
            .unwrap_or_default();
        let started_ms = self.clock.now_ms();
        let record = ModelIoRecord {
            facts: self.facts.clone(),
            attempt,
            request_id,
            started_ms,
            completed_ms: started_ms,
            method: match http.method {
                Method::Get => "GET",
                Method::Post => "POST",
            },
            path: path_of(&http.url),
            headers: request_headers(&http.headers),
            body: http.body.clone(),
            response: None,
            skipped: Vec::new(),
            outcome: Outcome::Abandoned,
            error: None,
            retry_ms: None,
        };
        Tape {
            sink: self.sink.clone(),
            clock: self.clock.clone(),
            record: Some(record),
            status: None,
            text: None,
            capture: None,
        }
    }
}

/// 这次发送怎么结束。
pub(crate) enum End {
    Completed,
    Cancelled,
    /// 失败；要重发时带等待的毫秒数。
    Failed(ModelError, Option<u64>),
    /// 看门狗中止了流；静默重发时带 0。
    Idle(ModelError, Option<u64>),
    /// 调用方在流结束之前丢掉了流。
    Abandoned,
}

impl End {
    /// 流里的错误：取消单独算。
    pub(crate) fn of_error(error: &ModelError) -> Self {
        match error {
            ModelError::Cancelled => Self::Cancelled,
            other => Self::Failed(other.clone(), None),
        }
    }
}

/// 结束一次可能没有录音的发送。
pub(crate) fn end(tape: Option<Tape>, end: End) {
    if let Some(mut tape) = tape {
        tape.emit(end);
    }
}

/// 一次发送的录音。丢弃时还没结束就按“流被丢弃”（或解码器已收到终止事件时“完成”）记一条。
pub(crate) struct Tape {
    sink: Arc<dyn ModelIoSink>,
    clock: Arc<dyn Clock>,
    record: Option<ModelIoRecord>,
    status: Option<(u16, Vec<(String, String)>)>,
    text: Option<String>,
    capture: Option<Arc<Mutex<Capture>>>,
}

impl Tape {
    /// 拿到 2xx：记下回复头，包住解码器。
    pub(crate) fn stream(
        &mut self,
        status: u16,
        headers: &ResponseHeaders,
        decoder: Box<dyn StreamDecoder>,
    ) -> Box<dyn StreamDecoder> {
        self.status = Some((status, response_headers(headers.pairs())));
        let capture = Capture::shared();
        let wrapped = Capture::wrap(&capture, decoder, Some(self.clock.clone()));
        self.capture = Some(capture);
        wrapped
    }

    /// 非流式的回复（非 2xx 的正文）。
    pub(crate) fn text(&mut self, status: u16, headers: &ResponseHeaders, body: &str) {
        self.status = Some((status, response_headers(headers.pairs())));
        self.text = Some(body.to_owned());
    }

    fn emit(&mut self, end: End) {
        let Some(mut record) = self.record.take() else {
            return;
        };
        let taken = self.capture.as_ref().map(|capture| lock(capture).take());
        let now = self.clock.now_ms();
        let (outcome, error, retry) = match end {
            End::Completed => (Outcome::Completed, None, None),
            End::Cancelled => (Outcome::Cancelled, Some(ModelError::Cancelled), None),
            End::Failed(error, retry) => (Outcome::Failed, Some(error), retry),
            End::Idle(error, retry) => (Outcome::IdleTimeout, Some(error), retry),
            End::Abandoned => (Outcome::Abandoned, None, None),
        };
        record.completed_ms = match (&outcome, taken.as_ref().and_then(|taken| taken.finished_ms)) {
            (Outcome::Completed, Some(at)) => at,
            _ => now,
        };
        if let Some((status, headers)) = self.status.take() {
            let (body, summary) = match (taken, self.text.take()) {
                (Some(taken), _) => {
                    record.skipped = taken.skipped;
                    (ResponseBody::Stream(taken.chunks), taken.summary)
                }
                (None, text) => (
                    ResponseBody::Text(text.unwrap_or_default()),
                    Summary::default(),
                ),
            };
            record.response = Some(ResponsePart {
                status,
                headers,
                body,
                summary,
            });
        }
        record.outcome = outcome;
        record.error = error;
        record.retry_ms = retry;
        self.sink.record(record);
    }
}

impl Drop for Tape {
    fn drop(&mut self) {
        if self.record.is_none() {
            return;
        }
        let finished = self
            .capture
            .as_ref()
            .is_some_and(|capture| lock(capture).finished);
        // 调用方收到终止事件之后不再读就丢掉了流：算完成；否则是中途丢弃。
        let end = if finished {
            End::Completed
        } else {
            End::Abandoned
        };
        self.emit(end);
    }
}

fn lock(capture: &Mutex<Capture>) -> MutexGuard<'_, Capture> {
    capture.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 包住解码器时收集的原料（解码器在读流的任务里，`Tape` 在发送方，两边共用）。
#[derive(Default)]
pub(crate) struct Capture {
    chunks: Vec<String>,
    /// 原始事件里的 `content_block_start`：（index，块的 type）。
    starts: Vec<(usize, String)>,
    /// 解码器产出过的块的 index。
    decoded: Vec<usize>,
    summary: Summary,
    finished: bool,
    finished_ms: Option<u64>,
}

/// [`Capture::take`] 的结果。
pub(crate) struct Taken {
    pub(crate) chunks: Vec<String>,
    pub(crate) skipped: Vec<(usize, String)>,
    pub(crate) summary: Summary,
    pub(crate) finished_ms: Option<u64>,
}

impl Capture {
    pub(crate) fn shared() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self::default()))
    }

    /// 包住 `inner`：解码器的行为不变。`clock` 给“收到终止事件”的时刻（单测没有）。
    pub(crate) fn wrap(
        capture: &Arc<Mutex<Self>>,
        inner: Box<dyn StreamDecoder>,
        clock: Option<Arc<dyn Clock>>,
    ) -> Box<dyn StreamDecoder> {
        Box::new(Recording {
            inner,
            capture: capture.clone(),
            clock,
        })
    }

    pub(crate) fn take(&mut self) -> Taken {
        let decoded = std::mem::take(&mut self.decoded);
        let skipped = std::mem::take(&mut self.starts)
            .into_iter()
            .filter(|(index, _)| !decoded.contains(index))
            .collect();
        Taken {
            chunks: std::mem::take(&mut self.chunks),
            skipped,
            summary: std::mem::take(&mut self.summary),
            finished_ms: self.finished_ms,
        }
    }

    fn decoded<'a>(&mut self, events: impl Iterator<Item = &'a ModelEvent>) {
        for event in events {
            match event {
                ModelEvent::BlockStart { index, block } => {
                    self.decoded.push(*index);
                    self.summary.reply.start(*index, block);
                }
                ModelEvent::BlockDelta { index, delta } => self.summary.reply.delta(*index, delta),
                ModelEvent::BlockStop { index } => self.summary.reply.stop(*index),
                ModelEvent::MessageStart { id, model, usage } => {
                    self.summary.id = Some(id.clone()).filter(|id| !id.is_empty());
                    self.summary.model = Some(model.clone()).filter(|model| !model.is_empty());
                    self.summary.usage = Some(*usage);
                }
                ModelEvent::MessageDelta { stop_reason, usage } => {
                    if stop_reason.is_some() {
                        self.summary.stop_reason = *stop_reason;
                    }
                    self.summary.usage = Some(*usage);
                }
                _ => {}
            }
        }
    }
}

/// 原始事件里的 `content_block_start`：（index，块的 type）。只在文本里有这个名字时才解析。
fn block_start(data: &str) -> Option<(usize, String)> {
    if !data.contains("content_block_start") {
        return None;
    }
    let value: Value = serde_json::from_str(data).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("content_block_start") {
        return None;
    }
    let index = usize::try_from(value.get("index")?.as_u64()?).ok()?;
    let kind = value
        .get("content_block")
        .and_then(|block| block.get("type"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Some((index, kind))
}

/// 原始事件里的 `input_json_delta`：（index，`partial_json`）。只在文本里有这个名字时才解析。
fn input_delta(data: &str) -> Option<(usize, String)> {
    if !data.contains("input_json_delta") {
        return None;
    }
    let value: Value = serde_json::from_str(data).ok()?;
    let delta = value.get("delta")?;
    if value.get("type").and_then(Value::as_str) != Some("content_block_delta")
        || delta.get("type").and_then(Value::as_str) != Some("input_json_delta")
    {
        return None;
    }
    let index = usize::try_from(value.get("index")?.as_u64()?).ok()?;
    Some((index, delta.get("partial_json")?.as_str()?.to_owned()))
}

struct Recording {
    inner: Box<dyn StreamDecoder>,
    capture: Arc<Mutex<Capture>>,
    clock: Option<Arc<dyn Clock>>,
}

impl StreamDecoder for Recording {
    fn decode(
        &mut self,
        event: SseEvent,
        out: &mut VecDeque<ModelEvent>,
    ) -> Result<(), ModelError> {
        let start = block_start(&event.data);
        let data = event.data.clone();
        let before = out.len();
        let result = self.inner.decode(event, out);
        let mut capture = lock(&self.capture);
        if let Some(start) = start {
            capture.starts.push(start);
        }
        capture.decoded(out.iter().skip(before));
        // 工作线 F93（model-io.md §4.2）：服务端调用的参数增量解码器不输出，从原始事件取。只在有服务端调用时解析。
        if capture.summary.reply.has_server_call()
            && let Some((index, partial)) = input_delta(&data)
        {
            capture.summary.reply.server_input(index, &partial);
        }
        capture.chunks.push(data);
        if !capture.finished && self.inner.finished() {
            capture.finished = true;
            capture.finished_ms = self.clock.as_ref().map(|clock| clock.now_ms());
        }
        result
    }

    fn finished(&self) -> bool {
        self.inner.finished()
    }
}
