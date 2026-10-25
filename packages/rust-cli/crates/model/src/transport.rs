//! 取端点、连接、重试、取消与读流（docs/specs/rust-cli/model.md §5）。与协议无关。
//!
//! 流是惰性的状态机（`stream::unfold`）：不轮询就不发请求，丢弃流即关闭连接，没有后台任务。
//!
//! ```text
//! Resolve（取端点与凭据，按协议编码）──▶ Connect ──2xx──▶ Read ──流结束事件──▶ 结束
//!                                          │ 失败且可重试：产出 Retrying 通知 ─▶ Wait（Retry-After / 退避）─▶ Connect
//!                                          │ 连续过载 3 次且有 fallback 模型 ─▶ Err(FallbackTriggered)
//!                                          └ 失败且不可重试 ──▶ Err ──▶ 结束      任一处取消 ──▶ Err(Cancelled)
//! ```
//!
//! 只重试拿到响应头之前的失败（重试只包住发请求）；
//! 流已经开始之后的错误直接交给调用方，由主循环决定怎么处理。唯一的例外是流空闲看门狗
//! （docs/specs/rust-cli/stall-visibility.md §2.3）：响应头之后一个事件都还没交给调用方就停住时，静默重发一次。
//!
//! 通知：等待重试前产出 `Retrying`；重发拿到 2xx、或停顿之后又收到事件时产出 `Resumed`；读流时 20 s 没有
//! 字节产出 `Stalled`（读流与看门狗在 `transport_read.rs`）。

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::model::{
    ModelError, ModelNotice, ModelRequest, ModelSelection, ModelStreamItem, QuerySource,
};
use rust_cli_kernel_api::provider::{ModelEndpoint, ModelEndpoints, WireProtocol};
use rust_cli_net::retry::{RetryPolicy, jitter};
use rust_cli_net::{Egress, HttpRequest};
use tokio_util::sync::CancellationToken;

use crate::attribution::Origin;
use crate::idle::{Limits, StreamIdle};
use crate::model_io::ModelIoSink;
use crate::model_io_tape::{End, Trail, end};
use crate::profiles::{self, ANTHROPIC_OFFICIAL_BASE_URL, Profile};
use crate::protocol::{self, Failure, Protocol, Retry};
use crate::transport_read::{Next, Read};

/// 错误响应正文最多读这么多字节（只用于分类与提示）。
const ERROR_BODY_LIMIT: usize = 64 * 1024;

/// 连续过载这么多次后，有 fallback 模型就换模型。
const MAX_OVERLOADED_BEFORE_FALLBACK: u32 = 3;

/// opus / fable / mythos 系列的主模型没有 fallback 时，连续过载后的错误文字。
pub(crate) const REPEATED_OVERLOAD: &str = "Repeated 529 Overloaded errors";

/// 哪些模型在连续过载后不再重试（模型目录里的系列是 opus、fable、
/// mythos）。本工程没有 Anthropic 的模型目录，按模型 ID 里的系列名判断（`claude-opus-5-5`、Bedrock 的
/// `us.anthropic.claude-opus-…` 等），其他模型照常按重试上限重试（model.md §5.2）。
fn stops_on_repeated_overload(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    ["claude-opus", "claude-fable", "claude-mythos"]
        .iter()
        .any(|family| model.contains(family))
}

/// 前台的旁路请求：用户在等结果，过载时也重试。
const FOREGROUND_SIDE_SOURCES: [&str; 5] = [
    "side_question",
    "auto_mode",
    "hook_agent",
    "hook_prompt",
    "verification_agent",
];

/// 网络出口、端点来源、重试策略、流空闲看门狗与时钟（Retry-After 的日期，F7）。
#[derive(Clone)]
pub(crate) struct Route {
    pub egress: Arc<Egress>,
    pub endpoints: Arc<dyn ModelEndpoints>,
    pub retry: RetryPolicy,
    pub idle: StreamIdle,
    pub clock: Arc<dyn Clock>,
    /// 入口声明“按 Anthropic 官方端点处理”（只给测试用，model.md §6）：按 Node 配置里官方 API 的地址查方案表。
    pub assume_first_party: bool,
    /// 设了 `IS_SANDBOX`（cli-args.md §9）：连续过载时不报 `Repeated 529 Overloaded errors`，照常重试。
    pub sandbox: bool,
    /// `ZCODE_MAX_OUTPUT_TOKENS`（cli-args.md §9.1，修复线 F42）：每个请求的 `max_tokens` 不超过它。
    pub max_output_tokens: Option<u32>,
    /// 设备身份的来源（工作线 F52）：`metadata.user_id` 的 `device_id`。
    pub device: Option<crate::metadata::DeviceIdSource>,
    /// 来源头与每请求头的 ID（工作线 F64，model.md §2.4；`user-agent` 是来源头的一项）；None 时这两层都不发。
    pub headers: Option<Arc<crate::request_headers::RequestHeaders>>,
    /// 模型轨迹的记录方（工作线 F91，model-io.md §6）；None 时不记。
    pub model_io: Option<Arc<dyn ModelIoSink>>,
}

impl Route {
    /// 这个端点的请求方案（model.md §3.2）。修复线 F23：原来按 `is_first_party`（只看主机名）分两种。
    pub(crate) fn profile(&self, endpoint: &ModelEndpoint) -> &'static Profile {
        let base_url = if self.assume_first_party {
            ANTHROPIC_OFFICIAL_BASE_URL
        } else {
            &endpoint.base_url
        };
        profiles::select(endpoint.protocol, base_url, &endpoint.model)
    }

    /// 记录方与时钟（没有记录方时 None）。
    pub(crate) fn recorder(&self) -> Option<(Arc<dyn ModelIoSink>, Arc<dyn Clock>)> {
        let sink = self.model_io.clone()?;
        Some((sink, self.clock.clone()))
    }

    /// 进程的 trace ID（每请求头的 `x-zcode-trace-id`）。
    pub(crate) fn trace(&self) -> Option<&str> {
        self.headers
            .as_deref()
            .map(|headers| headers.trace_id.as_str())
    }
}

/// 时长的毫秒数（记录与通知用）。
fn millis(delay: Duration) -> u64 {
    u64::try_from(delay.as_millis()).unwrap_or(u64::MAX)
}

/// 注入的时钟的当前时间（`SystemTime`，给 Retry-After 的 HTTP 日期换算）。
pub(crate) fn now(clock: &dyn Clock) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(clock.now_ms())
}

fn retries_overload(source: &QuerySource) -> bool {
    match source {
        QuerySource::MainThread | QuerySource::Subagent(_) | QuerySource::Compact => true,
        QuerySource::Side(name) => FOREGROUND_SIDE_SOURCES.contains(&name.as_str()),
    }
}

pub(crate) fn stream(
    route: &Route,
    request: ModelRequest,
    cancel: CancellationToken,
) -> BoxStream<'static, Result<ModelStreamItem, ModelError>> {
    let start = Start {
        route: route.clone(),
        request,
        cancel,
    };
    stream::unfold(State::Resolve(Box::new(start)), step).boxed()
}

enum State {
    Resolve(Box<Start>),
    Connect(Box<Connect>),
    Wait(Box<Connect>, Duration),
    Read(Box<Read>),
    Done,
}

async fn step(mut state: State) -> Option<(Result<ModelStreamItem, ModelError>, State)> {
    loop {
        state = match state {
            State::Done => return None,
            State::Resolve(start) => match start.resolve().await {
                Ok(connect) => State::Connect(Box::new(connect)),
                Err(error) => return Some((Err(error), State::Done)),
            },
            State::Connect(connect) => match connect.attempt().await {
                Outcome::Opened(read) => State::Read(read),
                Outcome::Retry(notice, connect, delay) => {
                    return Some((
                        Ok(ModelStreamItem::Notice(notice)),
                        State::Wait(connect, delay),
                    ));
                }
                Outcome::Failed(error) => return Some((Err(error), State::Done)),
            },
            State::Wait(connect, delay) => {
                match connect
                    .cancel
                    .run_until_cancelled(tokio::time::sleep(delay))
                    .await
                {
                    Some(()) => State::Connect(connect),
                    None => return Some((Err(ModelError::Cancelled), State::Done)),
                }
            }
            State::Read(mut read) => match read.next().await? {
                Next::Item(item) => {
                    let next = if item.is_err() {
                        State::Done
                    } else {
                        State::Read(read)
                    };
                    return Some((item, next));
                }
                Next::Idle(limit) => {
                    // 丢掉这次响应（关闭连接），按进度决定静默重发还是报错（stall-visibility.md §2.3）。
                    let delivered = read.delivered();
                    let (connect, tape) = read.into_parts();
                    let idle = ModelError::StreamIdle {
                        idle_ms: millis(limit),
                    };
                    return Some(match connect.idle(limit, delivered) {
                        Ok((notice, connect)) => {
                            end(tape, End::Idle(idle, Some(0)));
                            (Ok(ModelStreamItem::Notice(notice)), State::Connect(connect))
                        }
                        Err(error) => {
                            end(tape, End::Idle(idle, None));
                            (Err(error), State::Done)
                        }
                    });
                }
            },
        };
    }
}

/// 还没取端点的请求。
struct Start {
    route: Route,
    request: ModelRequest,
    cancel: CancellationToken,
}

impl Start {
    /// 取这次请求的端点（每请求鉴权），再按它的协议编码。
    async fn resolve(self) -> Result<Connect, ModelError> {
        let Start {
            route,
            mut request,
            cancel,
        } = self;
        if cancel.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        // 工作线 F95（model.md §9）：请求交给 Provider 补丁的输出上限随取端点交给平台层（目录模型的 `maxOutputTokens`
        // 补丁以它与配置的上限的较小值为输入）；没有时同 `endpoint`。
        let output_limit = request.options.provider_max_output_tokens.map(u64::from);
        let endpoint = cancel
            .run_until_cancelled(route.endpoints.endpoint_with_output_limit(
                &request.selection,
                None,
                output_limit,
            ))
            .await
            .ok_or(ModelError::Cancelled)??;
        // 工作线 F52（model.md §3.3）：每个请求都带 `metadata.user_id`。修 bug：以前只在调用方给了 user_id 时写，没有调用方
        // 给，请求里一直没有 metadata（Node 每个请求都写）。设备身份只在第一次请求时读文件。
        // 工作线 F52b（用户决定）：只给 Anthropic 协议。F52 三种协议都写，OpenAI 官方的 Chat Completions 在没有
        // `store: true` 时报 400；现在 OpenAI 两种协议不算 user_id、不读设备身份（Node 的 `providerKind` 判断相同）。
        if endpoint.protocol == WireProtocol::AnthropicMessages
            && request.options.user_id.is_none()
            && let Some(device) = &route.device
        {
            let device_id = cancel
                .run_until_cancelled(device())
                .await
                .ok_or(ModelError::Cancelled)?;
            let session = request
                .options
                .session_id
                .as_ref()
                .map(rust_cli_kernel_api::ids::SessionId::as_str);
            request.options.user_id = Some(crate::metadata::user_id(&device_id, session));
        }
        let protocol = protocol::for_wire(endpoint.protocol);
        // 每请求头要的会话、回合与来源在编码前取出（请求体只编码一次，重试复用；F64）。
        let origin = Origin::of(&request);
        let selection = request.selection.clone();
        let fallback = request.options.fallback_model.clone();
        let overload_retry = retries_overload(&request.source);
        // 模型不接受图片时换成占位文字（model.md §10.5），三种协议共用。
        crate::media::project(&mut request.messages, &endpoint);
        let profile = route.profile(&endpoint);
        tracing::debug!(
            event = "model.profile",
            model = %endpoint.model,
            profile = profile.name,
            "request profile selected"
        );
        // 模型轨迹（model-io.md §6）：请求的共用信息在编码之前取（编码会消费请求）。
        let trail = route.recorder().map(|recorder| {
            Trail::for_request(recorder, &request, &endpoint, profile.name, route.trace())
        });
        let encoded = protocol.encode(&endpoint, request, profile)?;
        let limits = route.idle.limits(profile.byte_idle);
        Ok(Connect {
            route,
            protocol,
            encoded,
            endpoint,
            origin,
            selection,
            fallback,
            overload_retry,
            attempt: 1,
            overloaded: 0,
            stalls: 0,
            notified: false,
            limits,
            cancel,
            trail,
        })
    }
}

/// 已编码的请求与重试计数。请求体只编码一次；凭据 header 每次发送前按当前端点写入。
pub(crate) struct Connect {
    route: Route,
    protocol: &'static dyn Protocol,
    encoded: HttpRequest,
    endpoint: ModelEndpoint,
    /// 发请求的一方（每请求头）。
    origin: Origin,
    selection: ModelSelection,
    fallback: Option<String>,
    overload_retry: bool,
    attempt: u32,
    /// 这次请求累计的过载次数（一次重试循环内只增不减）。
    overloaded: u32,
    /// 看门狗静默重发过几次（上限 1）。
    stalls: u32,
    /// 发过 `Retrying` 通知、还没有产出 `Resumed`。
    notified: bool,
    /// 这个端点上的看门狗上限。
    limits: Limits,
    cancel: CancellationToken,
    /// 模型轨迹：每次发送开一个录音（model-io.md §6）。
    trail: Option<Trail>,
}

enum Outcome {
    Opened(Box<Read>),
    Retry(ModelNotice, Box<Connect>, Duration),
    Failed(ModelError),
}

impl Connect {
    /// 发一次请求。失败时决定结束、换 fallback 模型，还是等待后重发。每次发送记一条模型轨迹（model-io.md §6）。
    async fn attempt(mut self: Box<Self>) -> Outcome {
        // 每次发送都按当前端点合并各层的头：凭据续期之后换新，`x-request-id` 每次换新（F64）。
        let http = crate::request_headers::outbound(
            self.protocol,
            &self.encoded,
            &self.endpoint,
            self.route.headers.as_deref(),
            &self.origin,
        );
        let mut tape = self
            .trail
            .as_ref()
            .map(|trail| trail.tape(self.attempt, &http));
        let Some(sent) = self
            .cancel
            .run_until_cancelled(self.route.egress.send(http))
            .await
        else {
            end(tape, End::Cancelled);
            return Outcome::Failed(ModelError::Cancelled);
        };
        let failure = match sent {
            Ok(response) if response.is_success() => {
                // 前面发过重试通知：拿到响应头就清掉它（重试循环结束时清掉重试状态）。
                let resumed = std::mem::replace(&mut self.notified, false);
                let mut decoder = self.protocol.decoder();
                if let Some(tape) = &mut tape {
                    decoder = tape.stream(response.status(), response.headers(), decoder);
                }
                let read = Read::new(response, decoder, self.limits, resumed, self, tape);
                return Outcome::Opened(Box::new(read));
            }
            Ok(response) => {
                let status = response.status();
                let headers = response.headers().clone();
                match self
                    .cancel
                    .run_until_cancelled(response.text(ERROR_BODY_LIMIT))
                    .await
                {
                    None => {
                        end(tape, End::Cancelled);
                        return Outcome::Failed(ModelError::Cancelled);
                    }
                    Some(Ok(body)) => {
                        if let Some(tape) = &mut tape {
                            tape.text(status, &headers, &body);
                        }
                        self.protocol.classify(
                            status,
                            &headers,
                            &body,
                            now(self.route.clock.as_ref()),
                        )
                    }
                    Some(Err(error)) => Failure::network(&error),
                }
            }
            Err(error) => Failure::network(&error),
        };
        let delay = match self.retry_delay(&failure) {
            Ok(delay) => {
                end(
                    tape,
                    End::Failed(failure.error.clone(), Some(millis(delay))),
                );
                delay
            }
            Err(error) => {
                end(tape, End::Failed(failure.error, None));
                return Outcome::Failed(error);
            }
        };
        if matches!(failure.error, ModelError::Authentication { .. }) && self.endpoint.renewable {
            // 每请求取的凭据被拒：带上被拒的端点再取一次（401 时刷新凭据再重试）。
            // 只换凭据与 Provider header；请求体已按原端点编码，不再改。
            let renewed = self
                .route
                .endpoints
                .endpoint(&self.selection, Some(&self.endpoint));
            match self.cancel.run_until_cancelled(renewed).await {
                None => return Outcome::Failed(ModelError::Cancelled),
                Some(Err(error)) => return Outcome::Failed(error),
                Some(Ok(endpoint)) => {
                    self.endpoint.credential = endpoint.credential;
                    self.endpoint.headers = endpoint.headers;
                }
            }
        }
        let delay_ms = millis(delay);
        let max_retries = self.route.retry.max_retries;
        // 带会话的 `model.retry`（warn）由 query 从重试通知记；这里是所有调用方的细节（logging.md §4）。
        tracing::debug!(
            event = "model.attempt.failed",
            model = %self.selection.model,
            attempt = self.attempt,
            max_retries,
            delay_ms,
            error = %failure.error,
            "model request failed; retrying"
        );
        let notice = ModelNotice::Retrying {
            attempt: self.attempt,
            max_retries,
            delay_ms,
            error: failure.error,
        };
        self.attempt += 1;
        self.notified = true;
        Outcome::Retry(notice, self, delay)
    }

    /// 失败之后：要重发时返回等待时长；否则返回给调用方的错误（换 fallback、连续过载、不可重试或到了上限）。
    fn retry_delay(&mut self, failure: &Failure) -> Result<Duration, ModelError> {
        let retryable = match failure.retry {
            Retry::Never => false,
            Retry::Transient => true,
            // 旁路请求过载时直接失败（后台请求不在容量紧张时放大重试）。
            Retry::Overloaded if !self.overload_retry => false,
            Retry::Overloaded => {
                self.overloaded += 1;
                if self.overloaded >= MAX_OVERLOADED_BEFORE_FALLBACK {
                    if let Some(fallback) = self.fallback.take() {
                        return Err(ModelError::FallbackTriggered {
                            original_model: self.selection.model.clone(),
                            fallback_model: fallback,
                        });
                    }
                    // 修 bug（F10）：以前没有 fallback 时一律重试到上限（11 次请求、约 2.5 分钟）；应对
                    // opus 系列在第 3 次过载后报 `Repeated 529 Overloaded errors`。
                    if !self.route.sandbox && stops_on_repeated_overload(&self.selection.model) {
                        return Err(ModelError::Overloaded {
                            message: REPEATED_OVERLOAD.to_owned(),
                        });
                    }
                }
                true
            }
        };
        let retry = &self.route.retry;
        let delay = if retryable && self.attempt <= retry.max_retries {
            retry.delay(self.attempt, failure.retry_after, jitter())
        } else {
            None
        };
        delay.ok_or_else(|| failure.error.clone())
    }

    pub(crate) fn cancel(&self) -> &CancellationToken {
        &self.cancel
    }

    /// 看门狗中止了这次流。调用方还没收到这次响应的任何事件、也还没静默重发过时，回到发送
    /// （计入重试次数，不等待）；否则报 `StreamIdle`，由主循环按进度收尾（stall-visibility.md §2.3）。
    fn idle(
        mut self: Box<Self>,
        limit: Duration,
        delivered: bool,
    ) -> Result<(ModelNotice, Box<Self>), ModelError> {
        let error = ModelError::StreamIdle {
            idle_ms: millis(limit),
        };
        let max_retries = self.route.retry.max_retries;
        if delivered || self.stalls > 0 || self.attempt > max_retries {
            return Err(error);
        }
        self.stalls += 1;
        tracing::debug!(
            event = "model.attempt.failed",
            model = %self.selection.model,
            attempt = self.attempt,
            max_retries,
            delay_ms = 0_u64,
            error = %error,
            "model stream idle; retrying"
        );
        let notice = ModelNotice::Retrying {
            attempt: self.attempt,
            max_retries,
            delay_ms: 0,
            error,
        };
        self.attempt += 1;
        self.notified = true;
        Ok((notice, self))
    }
}
