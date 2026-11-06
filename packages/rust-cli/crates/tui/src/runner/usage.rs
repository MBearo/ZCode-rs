//! `/usage` 在 runner 里的部分（docs/specs/rust-cli/usage.md §4、§5）：打开时一起问内核要会话的 API 时长与全时段的
//! 统计，都到了再打开页面；换日期范围时再查一次，回执带页面的请求序号，过时的由页面丢掉。footer 的请求统计
//! （tui-display.md §2.6，F27）也从用量表查：请求完成、回合结束、打开或切换会话时查一次，回执按序号收下。
use futures_util::FutureExt;
use futures_util::future::join;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::metering::{ActivityRange, ActivityStats, RequestStats, SessionMetering};
use rust_cli_kernel_api::session_api::KernelError;
use rust_cli_kernel_api::store::StoreError;

use super::Loop;
use super::session::Done;
use crate::ui::i18n;
use crate::ui::usage_plans::{PlanAnswer, PlanAsk};
use crate::view::SubagentFigures;

/// `/usage` 的查询结果。
pub(super) enum UsageDone {
    Opened {
        metering: Result<SessionMetering, StoreError>,
        all: Box<Result<ActivityStats, KernelError>>,
    },
    Range {
        seq: u64,
        stats: Box<Result<ActivityStats, KernelError>>,
    },
    /// footer 的请求统计（F27）。
    Footer {
        seq: u64,
        stats: Result<RequestStats, StoreError>,
    },
    /// 子代理的数字（`refresh_figures`）：请求序号，与每个子会话的数字。
    Figures {
        seq: u64,
        session: Option<String>,
        figures: Vec<(SessionId, SubagentFigures)>,
    },
    /// 失败回合 `row` 的上限（`usage_back`）：问的时候的会话与停住套餐的池与它回来的时刻。
    Back {
        session: Option<String>,
        row: u64,
        result: Result<Option<(String, f64)>, String>,
    },
}

impl Loop<'_> {
    pub(super) fn open_usage(&mut self) {
        let kernel = self.kernel.clone();
        let session = self.outbox.session().cloned();
        self.pending.push(
            async move {
                // 还没有会话就没有请求：API 时长是 0。
                let metering = async {
                    match &session {
                        Some(session) => kernel.queries().metering(session.id()).await,
                        None => Ok(SessionMetering::default()),
                    }
                };
                let (metering, all) = join(metering, kernel.activity(ActivityRange::All)).await;
                Done::Usage(UsageDone::Opened {
                    metering,
                    all: Box::new(all),
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
        // 数字的卡或上下文的页开着时，上下文的计数同一个时刻再问（tui-panels.md §4.1）。
        self.count_context();
        self.session_facts();
        if self.outbox.session().is_none() {
            return;
        }
        let Some(session) = self.app.stats_session() else {
            return;
        };
        // 看的会话已经结束的回合数：速度的历史按它给回合收尾（kernel-api `metering::SpeedHistory`）。
        let ended = u64::from(self.app.shown().turn_totals().0);
        let seq = self.app.snapshots.request_stats.begin(ended);
        let kernel = self.kernel.clone();
        self.pending.push(
            async move {
                let stats = kernel.queries().request_stats(&session).await;
                Done::Usage(UsageDone::Footer { seq, stats })
            }
            .boxed(),
        );
    }

    /// 子代理的数字（Puffin `Query::Subagents`，cli 的 Puffin host 的 `figures`；tui-panels.md §1.3）：子代理开始或结束、
    /// 子代理快照的版本或状态变了时，问内核每个子会话的请求统计：主会话里 Agent 调用派生的，与快照里的全部子代理（含
    /// 子代理派生的、后台的）。回执带序号与会话，比已收下的旧的回执与别的会话的回执丢掉。
    pub(super) fn refresh_figures(&mut self) {
        let mut children: Vec<SessionId> = self.app.view.subagent_children().cloned().collect();
        children.extend(
            self.app
                .snapshots
                .agents
                .entries
                .iter()
                .map(|e| e.child.clone()),
        );
        children.sort();
        children.dedup();
        self.panels.figures_seq += 1;
        if children.is_empty() {
            return;
        }
        let seq = self.panels.figures_seq;
        let session = self.app.session.clone();
        let kernel = self.kernel.clone();
        self.pending.push(
            async move {
                let mut figures = Vec::with_capacity(children.len());
                for child in children {
                    if let Ok(stats) = kernel.queries().request_stats(&child).await {
                        figures.push((child, SubagentFigures::of(&stats)));
                    }
                }
                Done::Usage(UsageDone::Figures {
                    seq,
                    session,
                    figures,
                })
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
            } => self.app.open_usage(metering, shown(*all)),
            UsageDone::Opened {
                metering: Err(error),
                ..
            } => self.app.error(i18n::rejected(lang, &error.to_string())),
            UsageDone::Range { seq, stats } => self.app.usage_range(seq, shown(*stats)),
            // 晚到的旧回执不覆盖新的；存储不支持时没有值。
            UsageDone::Footer { seq, stats } => {
                self.app.snapshots.request_stats.arrived(seq, stats.ok());
            }
            // 问的时候的会话换了：行 id 属于原来的视图，丢掉。
            UsageDone::Back {
                session,
                row,
                result,
            } if session == self.app.session => {
                let back = result.unwrap_or_else(|why| {
                    tracing::warn!(
                        "usage: could not read the limit after turn failure {row}: {why}"
                    );
                    None
                });
                self.app.usage_back(row, back);
            }
            UsageDone::Back { .. } => {}
            // 晚到的旧回执与查的时候看的会话换了的回执都丢掉（数字属于原来那个）。
            UsageDone::Figures {
                seq,
                session,
                figures,
            } if seq > self.panels.figures_taken && session == self.app.session => {
                self.panels.figures_taken = seq;
                for (child, figures) in figures {
                    self.app.view.set_figures(child, figures);
                }
            }
            UsageDone::Figures { .. } => {}
        }
    }
}

/// 没有套餐用量的端口时的回答（cli Puffin host 的 `UNAVAILABLE`）。
const UNAVAILABLE: &str = "This isn't available in this version yet";

impl Loop<'_> {
    /// `/usage` 的卡与详情页的问题（usage_plans.rs）：经入口的端口问，每个问题一个 future；回答回到
    /// [`crate::ui::app::App::plan_answered`]。
    /// 套餐的上限停下了回合 `row`（Puffin `ask_back`）：问 `provider` 的计划；ZCode 跑着当前模型时再问第一个计划的额度，
    /// 用完的 5 小时或每周的池说它什么时候回来（kit `usage::page::wall`）。
    pub(super) fn usage_back(&mut self, row: u64, provider: String) {
        let port = self.options.services.plan_usage.clone();
        let session = self.app.session.clone();
        self.pending.push(
            async move {
                let result = async {
                    let port = port.ok_or_else(|| UNAVAILABLE.to_owned())?;
                    let (model, plans) = port.plans(provider).await?;
                    match plans.first().filter(|_| model.official) {
                        Some(p) => Ok(rust_cli_tui_kit::app::widgets::usage::page::wall(
                            &port.quota(p.id.clone()).await?,
                        )),
                        None => Ok(None),
                    }
                }
                .await;
                Done::Usage(UsageDone::Back {
                    session,
                    row,
                    result,
                })
            }
            .boxed(),
        );
    }

    pub(super) fn plan_usage(&mut self, asks: Vec<PlanAsk>) {
        let port = self.options.services.plan_usage.clone();
        for ask in asks {
            let port = port.clone();
            self.pending.push(
                async move {
                    let answer = match ask {
                        PlanAsk::Plans { provider } => PlanAnswer::Plans {
                            result: match &port {
                                Some(port) => port.plans(provider.clone()).await,
                                None => Err(UNAVAILABLE.to_owned()),
                            },
                            provider,
                        },
                        PlanAsk::Quota { plan } => PlanAnswer::Quota {
                            result: match &port {
                                Some(port) => port.quota(plan.clone()).await,
                                None => Err(UNAVAILABLE.to_owned()),
                            },
                            plan,
                        },
                        PlanAsk::Detail { plan, range } => PlanAnswer::Detail {
                            result: match &port {
                                Some(port) => port.detail(plan.clone(), range).await.map(Box::new),
                                None => Err(UNAVAILABLE.to_owned()),
                            },
                            plan,
                            range,
                        },
                    };
                    Done::PlanUsage(answer)
                }
                .boxed(),
            );
        }
    }
}
