//! `/usage` 在 runner 里的部分（docs/specs/rust-cli/usage.md §4、§5）：打开时一起问内核要会话的 API 时长与全时段的
//! 统计，都到了再打开页面；换日期范围时再查一次，回执带页面的请求序号，过时的由页面丢掉。footer 的请求统计
//! （tui-display.md §2.6，F27）也从用量表查：请求完成、回合结束、打开或切换会话时查一次，回执按序号收下。
use futures_util::FutureExt;
use futures_util::future::join;
use ratatui::backend::Backend;
use rust_cli_kernel_api::metering::{ActivityRange, ActivityStats, RequestStats, SessionMetering};
use rust_cli_kernel_api::session_api::KernelError;

use super::Loop;
use super::session::Done;
use crate::ui::i18n;

/// `/usage` 的查询结果。
pub(super) enum UsageDone {
    Opened {
        metering: Result<SessionMetering, KernelError>,
        all: Box<Result<ActivityStats, KernelError>>,
        stats: bool,
    },
    Range {
        seq: u64,
        stats: Box<Result<ActivityStats, KernelError>>,
    },
    /// footer 的请求统计（F27）。
    Footer {
        seq: u64,
        stats: Result<RequestStats, KernelError>,
    },
}

impl<B: Backend> Loop<'_, B> {
    pub(super) fn open_usage(&mut self, stats: bool) {
        let kernel = self.kernel.clone();
        let session = self.session.clone();
        self.pending.push(
            async move {
                // 还没有会话就没有请求：API 时长是 0。
                let metering = async {
                    match &session {
                        Some(session) => kernel.session_metering(session.id()).await,
                        None => Ok(SessionMetering::default()),
                    }
                };
                let (metering, all) = join(metering, kernel.activity(ActivityRange::All)).await;
                Done::Usage(UsageDone::Opened {
                    metering,
                    all: Box::new(all),
                    stats,
                })
            }
            .boxed(),
        );
    }

    pub(super) fn usage_range(&mut self, range: ActivityRange, seq: u64) {
        let kernel = self.kernel.clone();
        self.pending.push(
            async move {
                let stats = Box::new(kernel.activity(range).await);
                Done::Usage(UsageDone::Range { seq, stats })
            }
            .boxed(),
        );
    }

    /// footer 的请求统计（tui-display.md §2.6）：存储是唯一来源，每次都重查，不在界面里累加；没有会话时不查。查的是现在
    /// 看的那一个（修复线 F112，tui-panels.md §1.7）：主会话，或正在查看的子代理的子会话（`App::stats_session`）。
    pub(super) fn refresh_stats(&mut self) {
        if self.session.is_none() {
            return;
        }
        let Some(session) = self.app.stats_session() else {
            return;
        };
        let seq = self.app.request_stats.begin();
        let kernel = self.kernel.clone();
        self.pending.push(
            async move {
                let stats = kernel.request_stats(&session).await;
                Done::Usage(UsageDone::Footer { seq, stats })
            }
            .boxed(),
        );
    }

    pub(super) fn on_usage(&mut self, done: UsageDone) {
        let lang = self.app.lang;
        let shown = |result: Result<ActivityStats, KernelError>| {
            result.map_err(|error| i18n::rejected(lang, &error.to_string()))
        };
        match done {
            UsageDone::Opened {
                metering: Ok(metering),
                all,
                stats,
            } => self.app.open_usage(metering, shown(*all), stats),
            UsageDone::Opened {
                metering: Err(error),
                ..
            } => self.app.error(i18n::rejected(lang, &error.to_string())),
            UsageDone::Range { seq, stats } => self.app.usage_range(seq, shown(*stats)),
            // 晚到的旧回执不覆盖新的；存储不支持时没有值。
            UsageDone::Footer { seq, stats } => {
                self.app.request_stats.arrived(seq, stats.ok());
            }
        }
    }
}
