//! 模型轨迹的记录（docs/specs/rust-cli/model-io.md §4、§5）：一次 HTTP 发送一条记录。
//!
//! 记录在请求路径上只保存原料（请求体的 `Bytes`、每个 SSE 事件的 `data` 文本、脱敏后的头）；解析、脱敏请求体与
//! 拼成 JSON 在 [`ModelIoRecord::into_json`] 里做，由入口的写者在自己的线程上调用。

use std::borrow::Cow;
use std::sync::Arc;

use rust_cli_base::js::iso_timestamp;
use rust_cli_base::redact::redact_secrets;
use rust_cli_kernel_api::message::{StopReason, Usage};
use rust_cli_kernel_api::model::ModelError;
use rust_cli_kernel_api::provider::WireProtocol;
use rust_cli_net::{Bytes, HeaderValue};
use serde_json::{Map, Value, json};

use crate::model_io_reply::{self, Reply};

/// 记录的类型（Node `type: "model_io"`，App 的读者只认它）。
const RECORD_TYPE: &str = "model_io";
/// 秘密头的占位（Node `HEADER_REDACTION_VALUE`）。
const HEADER_REDACTED: &str = "[redacted]";
/// `metadata.user_id` 的占位（Node `REDACTED_METADATA_USER_ID`）。
const USER_ID_REDACTED: &str = "[REDACTED]";
/// 一律脱敏的头（Node `redactedHeaderNames`）。
const SECRET_HEADERS: [&str; 8] = [
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "api-key",
    "openai-api-key",
    "x-off-peak-ticket-id",
];
/// 名字含这些片段的头也脱敏（Node `shouldRedactHeader`）。
const SECRET_HEADER_PARTS: [&str; 5] = ["authorization", "api-key", "token", "secret", "cookie"];
/// 回复头里服务端给的请求 ID（Anthropic），记作 `response.requestId`；没有时取 `x-request-id`。
const RESPONSE_REQUEST_ID: &str = "request-id";
/// 流结束的标记（OpenAI 两种协议），不记（对齐基准的 dump 同样跳过）。
const DONE: &str = "[DONE]";

/// 模型轨迹的接收方（入口实现：有界队列加写者任务）。只做入队，不等 IO，不能阻塞。
pub trait ModelIoSink: Send + Sync {
    fn record(&self, record: ModelIoRecord);
}

/// 一次请求的各次发送共用的信息（取端点之后算一次）。
#[derive(Debug)]
pub(crate) struct RequestFacts {
    pub(crate) session: Option<String>,
    pub(crate) turn: Option<String>,
    pub(crate) trace: Option<String>,
    pub(crate) query_source: String,
    /// 轨迹比对基线（docs/specs/rust-cli/model-io.md §2.4）的请求类别：`main` / `subagent` / `side`。
    pub(crate) class: &'static str,
    /// 轨迹比对基线的路由：`messages` / `chat_completions` / `responses`（工作线 F93 删掉计数之前还有 `count_tokens`）。
    pub(crate) route: &'static str,
    pub(crate) model: String,
    pub(crate) provider: String,
    pub(crate) protocol: WireProtocol,
    pub(crate) profile: &'static str,
}

/// 这次发送的结局（model-io.md §4.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Completed,
    Failed,
    Cancelled,
    IdleTimeout,
    Abandoned,
}

impl Outcome {
    fn name(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::IdleTimeout => "idle_timeout",
            Self::Abandoned => "abandoned",
        }
    }
}

/// 回复正文：流式时是每个 SSE 事件的 `data`，否则是（截断的）正文。
#[derive(Debug)]
pub(crate) enum ResponseBody {
    Stream(Vec<String>),
    Text(String),
}

/// 从解码后的事件取的汇总；`reply` 是 Node 形状的回复字段的原料（工作线 F93，model-io.md §4.2）。
#[derive(Debug, Default, Clone)]
pub(crate) struct Summary {
    pub(crate) id: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) stop_reason: Option<StopReason>,
    pub(crate) usage: Option<Usage>,
    pub(crate) reply: Reply,
}

#[derive(Debug)]
pub(crate) struct ResponsePart {
    pub(crate) status: u16,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: ResponseBody,
    pub(crate) summary: Summary,
}

/// 一次 HTTP 发送的记录。
#[derive(Debug)]
pub struct ModelIoRecord {
    pub(crate) facts: Arc<RequestFacts>,
    pub(crate) attempt: u32,
    pub(crate) request_id: String,
    pub(crate) started_ms: u64,
    pub(crate) completed_ms: u64,
    pub(crate) method: &'static str,
    pub(crate) path: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Bytes,
    pub(crate) response: Option<ResponsePart>,
    pub(crate) skipped: Vec<(usize, String)>,
    pub(crate) outcome: Outcome,
    pub(crate) error: Option<ModelError>,
    pub(crate) retry_ms: Option<u64>,
}

impl ModelIoRecord {
    /// 发这次请求的会话（文件按它分，model-io.md §3.2）。
    pub fn session(&self) -> Option<&str> {
        self.facts.session.as_deref()
    }

    /// 发出请求的时刻（注入的时钟，毫秒）。
    pub fn started_ms(&self) -> u64 {
        self.started_ms
    }

    /// 记录的 JSON（键序见 model-io.md §4.1）。解析请求体与回复、脱敏；写者在阻塞线程上调用。
    pub fn into_json(self) -> Map<String, Value> {
        let facts = &self.facts;
        let mut out = Map::new();
        out.insert("type".into(), RECORD_TYPE.into());
        out.insert("requestId".into(), self.request_id.into());
        out.insert("attempt".into(), self.attempt.into());
        out.insert("startedAt".into(), iso_timestamp(self.started_ms).into());
        out.insert(
            "completedAt".into(),
            iso_timestamp(self.completed_ms).into(),
        );
        out.insert(
            "durationMs".into(),
            self.completed_ms.saturating_sub(self.started_ms).into(),
        );
        let optional = [
            ("sessionId", &facts.session),
            ("turnId", &facts.turn),
            ("traceId", &facts.trace),
        ];
        for (key, value) in optional {
            if let Some(value) = value {
                out.insert(key.into(), value.clone().into());
            }
        }
        out.insert("querySource".into(), facts.query_source.clone().into());
        out.insert(
            "model".into(),
            json!({
                "modelId": facts.model,
                "providerId": facts.provider,
                "protocol": facts.protocol,
                "profile": facts.profile,
            }),
        );
        let mut request = Map::new();
        request.insert("route".into(), facts.route.into());
        request.insert("class".into(), facts.class.into());
        request.insert("method".into(), self.method.into());
        request.insert("path".into(), self.path.into());
        request.insert("headers".into(), header_object(self.headers));
        request.insert("body".into(), redact_body(parse_or_text(&self.body)));
        out.insert("request".into(), Value::Object(request));
        if let Some(response) = self.response {
            out.insert("response".into(), response_json(response));
        }
        if !self.skipped.is_empty() {
            let blocks: Vec<Value> = self
                .skipped
                .into_iter()
                .map(|(index, kind)| json!({"index": index, "type": kind}))
                .collect();
            out.insert("decoder".into(), json!({"skippedBlocks": blocks}));
        }
        out.insert("outcome".into(), self.outcome.name().into());
        if let Some(error) = &self.error {
            out.insert("error".into(), error_json(error));
        }
        if let Some(delay) = self.retry_ms {
            out.insert("retry".into(), json!({"delayMs": delay}));
        }
        out
    }
}

fn response_json(response: ResponsePart) -> Value {
    let mut out = Map::new();
    out.insert("status".into(), response.status.into());
    let request_id = header_value(&response.headers, RESPONSE_REQUEST_ID)
        .or_else(|| header_value(&response.headers, crate::attribution::REQUEST_ID));
    out.insert("headers".into(), header_object(response.headers));
    if let Some(id) = request_id {
        out.insert("requestId".into(), id.into());
    }
    let streamed = matches!(response.body, ResponseBody::Stream(_));
    let data = match response.body {
        ResponseBody::Stream(chunks) => {
            let chunks: Vec<Value> = chunks
                .iter()
                .filter(|data| data.as_str() != DONE)
                .map(|data| parse_or_text(data.as_bytes()))
                .collect();
            json!({"stream": true, "chunks": chunks})
        }
        ResponseBody::Text(text) => parse_or_text(text.as_bytes()),
    };
    out.insert("data".into(), data);
    let summary = response.summary;
    if let Some(id) = summary.id {
        out.insert("responseId".into(), id.into());
    }
    if let Some(model) = summary.model {
        out.insert("modelId".into(), model.into());
    }
    // 修改（工作线 F93，model-io.md §4.2）：App 读的回复字段照 Node 的形状——停止原因换成 AI SDK 的统一值、用量换成 Node 的
    // 键名，另写由事件拼出的 `text`、`reasoningText`、`toolCalls`（只对流式回复）。F91 时是 Anthropic 的停止原因与用量原文，
    // 没有后三个，App 的面板读不到回复文字与工具调用。原值都在 `data.chunks` 里。
    if let Some(reason) = summary.stop_reason {
        out.insert(
            "finishReason".into(),
            model_io_reply::finish_reason(reason).into(),
        );
    }
    if streamed {
        summary.reply.write(&mut out);
    }
    if let Some(usage) = summary.usage {
        out.insert("usage".into(), model_io_reply::usage(usage));
    }
    Value::Object(out)
}

/// 模型错误：`name` 是种类（serde 的 `kind`），`message` 是它的文字（按值脱敏）。
fn error_json(error: &ModelError) -> Value {
    let name = serde_json::to_value(error)
        .ok()
        .and_then(|value| value.get("kind").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    json!({"name": name, "message": redact_secrets(&error.to_string())})
}

fn header_value(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

fn header_object(headers: Vec<(String, String)>) -> Value {
    Value::Object(
        headers
            .into_iter()
            .map(|(name, value)| (name, Value::String(value)))
            .collect(),
    )
}

/// 名字按 Node `sanitizeModelNetworkHeaders` 判断要不要整值脱敏（名字已是小写）。
fn secret_name(name: &str) -> bool {
    SECRET_HEADERS.contains(&name) || SECRET_HEADER_PARTS.iter().any(|part| name.contains(part))
}

/// 一个明文头的值：按名字整值脱敏，否则按值脱敏。
fn plain_header(name: &str, value: &str) -> String {
    if secret_name(name) {
        HEADER_REDACTED.to_owned()
    } else {
        redact_secrets(value).into_owned()
    }
}

/// 请求头：名字小写；秘密值一律脱敏。同名时后者替换前者（与发送时相同）。
pub(crate) fn request_headers(headers: &[(String, HeaderValue)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::with_capacity(headers.len());
    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        let value = match value {
            HeaderValue::Secret(_) => HEADER_REDACTED.to_owned(),
            HeaderValue::Plain(text) => plain_header(&name, text),
        };
        out.retain(|(existing, _)| *existing != name);
        out.push((name, value));
    }
    out
}

/// 回复头（名字已是小写）。
pub(crate) fn response_headers(pairs: Vec<(String, String)>) -> Vec<(String, String)> {
    pairs
        .into_iter()
        .map(|(name, value)| {
            let value = plain_header(&name, &value);
            (name, value)
        })
        .collect()
}

/// URL 的路径加查询串（轨迹比对基线的 `path`），按值脱敏。
pub(crate) fn path_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = rest.find('/').map_or("/", |at| &rest[at..]);
    redact_secrets(path).into_owned()
}

fn parse_or_text(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap_or_else(|_| {
        Value::String(redact_secrets(&String::from_utf8_lossy(bytes)).into_owned())
    })
}

/// 请求体的脱敏（Node `redactAnthropicRequestMetadata` 与 `redactModelIOImageAndVideoData`）。
pub(crate) fn redact_body(mut body: Value) -> Value {
    if let Some(user) = body
        .get_mut("metadata")
        .and_then(|metadata| metadata.get_mut("user_id"))
    {
        *user = Value::String(USER_ID_REDACTED.to_owned());
    }
    redact_media(&mut body);
    body
}

/// 图片与视频的原始数据换成占位文字：`{type: "base64", media_type}` 的 `data`、`{type: "image" | "file", mediaType}`
/// 的 `image` / `data`，以及任何 data URL。
fn redact_media(value: &mut Value) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(redact_media),
        Value::Object(object) => {
            let kind = object.get("type").and_then(Value::as_str);
            let raw_type = match kind {
                Some("base64") => object.get("media_type").and_then(Value::as_str),
                Some("image" | "file") => object.get("mediaType").and_then(Value::as_str),
                _ => None,
            }
            .filter(|media| is_image_or_video(media))
            .map(str::to_owned);
            let raw_key = if kind == Some("image") {
                "image"
            } else {
                "data"
            };
            for (key, child) in object.iter_mut() {
                if let Value::String(text) = child {
                    if let Some(media) =
                        data_url_type(text).filter(|media| is_image_or_video(media))
                    {
                        *child = omitted(key, &media, text.encode_utf16().count());
                        continue;
                    }
                    if key == raw_key
                        && let Some(media) = &raw_type
                    {
                        *child = omitted(key, media, text.encode_utf16().count());
                        continue;
                    }
                }
                redact_media(child);
            }
        }
        _ => {}
    }
}

fn omitted(key: &str, media: &str, length: usize) -> Value {
    Value::String(format!(
        "[{key} omitted from model-io: {media}, {length} chars]"
    ))
}

/// `data:<type>[;,]` 的类型。
fn data_url_type(text: &str) -> Option<String> {
    let rest = text.strip_prefix("data:")?;
    let end = rest.find([';', ','])?;
    (end > 0).then(|| rest[..end].to_owned())
}

fn is_image_or_video(media: &str) -> bool {
    let media: Cow<'_, str> = Cow::Owned(media.trim().to_ascii_lowercase());
    media.starts_with("image/") || media.starts_with("video/")
}

#[cfg(test)]
#[path = "model_io_tests.rs"]
mod tests;
