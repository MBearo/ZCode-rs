//! 一次失败之后：按处置表的动作决定等多久再发、走 G，还是把错误交给调用方（docs/specs/rust-cli/provider-errors.md §2.4、
//! §3.2、§4，修复线 F113）。响应头之前（P0）与交出边界之前（P1）共用这一处；计数都在 [`Connect`]，每个只由这里改。

use std::time::Duration;

use rust_cli_kernel_api::model::{ModelError, ModelNotice};
use rust_cli_net::retry::jitter;

use super::{
    Connect, MAX_OVERLOADED_BEFORE_FALLBACK, REPEATED_OVERLOAD, millis, stops_on_repeated_overload,
};
use crate::errors::dispose::{Action, Exhausted, Phase, dispose};
use crate::errors::{Class, Verdict};
use crate::model_io_tape::{End, Tape, end};

/// 账号或套餐的并发上限最多重试这么多次（D5 暂定）。
const CAPACITY_RETRIES: u32 = 2;

/// 交出边界之前的截断最多重试这么多次，之后走 G（provider-errors.md D17 暂定）。
const TRUNCATION_RETRIES: u32 = 1;

impl Connect {
    /// 一次失败：要重发时返回重试通知与等待时长（这次发送的模型轨迹记 `Failed` 带等待）；否则返回给调用方的错误。
    /// 凭据被拒、端点可续期时先续期（只换凭据与 Provider header；请求体已按原端点编码，不再改）。
    pub(super) async fn fail(
        mut self: Box<Self>,
        verdict: Verdict,
        phase: Phase,
        tape: Option<Tape>,
    ) -> Result<(ModelNotice, Box<Self>, Duration), ModelError> {
        let delay = match self.delay_for(&verdict, phase) {
            Ok(delay) => {
                end(
                    tape,
                    End::Failed(verdict.error.clone(), Some(millis(delay))),
                );
                delay
            }
            Err(error) => {
                end(tape, End::Failed(verdict.error, None));
                return Err(error);
            }
        };
        if verdict.class == Class::AuthRejected && self.endpoint.renewable {
            // 每请求取的凭据被拒：带上被拒的端点再取一次（401 时刷新凭据再重试）。
            let renewed = self.route.endpoints.endpoint_in(
                &self.selection,
                Some(&self.endpoint),
                self.output_limit,
                &self.context,
            );
            match self.cancel.run_until_cancelled(renewed).await {
                None => return Err(ModelError::Cancelled),
                Some(Err(error)) => return Err(error),
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
            phase = ?phase,
            class = ?verdict.class,
            error = %verdict.error,
            "model request failed; retrying"
        );
        let notice = ModelNotice::Retrying {
            attempt: self.attempt,
            max_retries,
            delay_ms,
            error: verdict.error,
        };
        self.attempt += 1;
        self.notified = true;
        Ok((notice, self, delay))
    }

    /// 处置表的动作换成等待时长；不再重发时是给调用方的错误（换 fallback、连续过载、不可重试或到了上限）。
    fn delay_for(&mut self, verdict: &Verdict, phase: Phase) -> Result<Duration, ModelError> {
        let failed = || verdict.error.clone();
        let action = dispose(
            verdict.class,
            phase,
            self.overload_retry,
            verdict.should_retry,
        );
        let delay = match action {
            Action::Fail => None,
            Action::Resend => self.resend(),
            Action::Retry { exhausted } => self.backoff(verdict).or_else(|| match exhausted {
                Exhausted::Resend => self.resend(),
                Exhausted::Fail => None,
            }),
            Action::Capacity => {
                self.capacity += 1;
                if self.capacity > CAPACITY_RETRIES {
                    None
                } else {
                    self.backoff(verdict)
                }
            }
            Action::Truncation => {
                self.truncations += 1;
                if self.truncations > TRUNCATION_RETRIES {
                    self.resend()
                } else {
                    self.backoff(verdict).or_else(|| self.resend())
                }
            }
            Action::Overload => {
                self.overloaded += 1;
                if self.overloaded >= MAX_OVERLOADED_BEFORE_FALLBACK {
                    if let Some(fallback) = self.fallback.take() {
                        return Err(ModelError::FallbackTriggered {
                            original_model: self.selection.model.clone(),
                            fallback_model: fallback,
                        });
                    }
                    match phase {
                        // 修 bug（F10）：以前没有 fallback 时一律重试到上限（11 次请求、约 2.5 分钟）；应对 opus 系列在
                        // 第 3 次过载后报 `Repeated 529 Overloaded errors`。
                        Phase::Head
                            if !self.route.sandbox
                                && stops_on_repeated_overload(&self.selection.model) =>
                        {
                            return Err(ModelError::Overloaded {
                                message: REPEATED_OVERLOAD.to_owned(),
                            });
                        }
                        Phase::Head => {}
                        // 流内第 3 次过载、没有 fallback：走 G（代替非流式重发），不看模型系列。
                        Phase::Stream => return self.resend().ok_or_else(failed),
                    }
                }
                match phase {
                    Phase::Head => self.backoff(verdict),
                    Phase::Stream => self.backoff(verdict).or_else(|| self.resend()),
                }
            }
        };
        delay.ok_or_else(failed)
    }

    /// 退避等待（认 Retry-After；等待超过上限时不等）；次数用尽时为 None。
    fn backoff(&self, verdict: &Verdict) -> Option<Duration> {
        let retry = &self.route.retry;
        if self.attempt > retry.max_retries {
            return None;
        }
        retry.delay(self.attempt, verdict.retry_after, jitter())
    }

    /// G：一次流式重发代替非流式重发，不等待、不受次数上限限制；用过之后为 None（provider-errors.md §3.2）。
    pub(super) fn resend(&mut self) -> Option<Duration> {
        (!std::mem::replace(&mut self.resent, true)).then_some(Duration::ZERO)
    }
}
