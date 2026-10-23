//! 插话在工具边界并入前的 UserPromptSubmit（docs/specs/rust-cli/query.md §4.1），与同一工具边界的后台通知
//! （§5，L18）。
//!
//! 队列与 next 通道只由 actor 持有：主循环经 [`ActorSink`] 先领取（不出队），在主循环的任务里跑 hook
//! （不挡 actor），再把每条插话的结局交回 actor 出队或留下。通知不跑 hook，提交之后才出通道（lane.rs）。
//! 领取与结局之间队列可能被改动，actor 只按命令 ID 处理还在队列里的项。

use std::sync::Arc;

use rust_cli_kernel_api::hooks::{HookEvent, HookEventName, HookOutcome, HookRunner};
use rust_cli_kernel_api::ids::CommandId;
use rust_cli_kernel_api::orchestration::{Absorbed, Steered};
use rust_cli_kernel_api::session_api::{KernelEvent, NoticeLevel};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::actor::Actor;
use crate::lane::Lane;
use crate::sink::{ActorSink, SinkRequest};

/// 领取到的一项，按通道顺序。
pub(crate) enum ClaimedItem {
    /// 插话，带组装好的 UPS 事件。
    Prompt(CommandId, String, Box<HookEvent>),
    /// 通知全文。
    Notice(String),
}

/// 领取到的插话与通知；没有 UPS hook 时 `hooks` 为 None。
pub(crate) struct Claimed {
    pub(crate) items: Vec<ClaimedItem>,
    pub(crate) hooks: Option<Arc<dyn HookRunner>>,
    pub(crate) abort: CancellationToken,
}

/// 一条插话的结局。
pub(crate) enum Fate {
    /// 并入本回合：出队。
    Absorbed,
    /// UPS 阻塞：出队，不并入；带给界面的提示（query.md §4.2）。
    Dropped(Option<String>),
    /// 留到回合结束后作为独立回合；有结果时那个回合直接用它，不再跑 hook。
    Waits(Option<Box<HookOutcome>>),
}

impl Actor {
    /// 按通道顺序给出插话与通知。遇到留下的插话后不再给插话（筛查屏障只挡 `prompt`），通知照常
    /// 给出。已中止或没有回合时不领取，都继续排队。
    pub(crate) fn claim_steering(&self) -> Option<Claimed> {
        let running = self.running.as_ref()?;
        if running.abort.is_aborted() {
            return None;
        }
        let mut items = Vec::new();
        let mut barrier = false;
        for item in &self.lane {
            match item {
                Lane::Input(command) => {
                    if barrier || self.screened.contains_key(command) {
                        barrier = true;
                        continue;
                    }
                    let Some(input) = self.queue.iter().find(|input| input.command == *command)
                    else {
                        continue;
                    };
                    // 斜杠命令不在工具边界并入，留在通道里，回合结束后作为独立回合展开（query.md §4，L22）；它不是屏障，
                    // 后面的插话照常领取。
                    if input.text.trim_start().starts_with('/') {
                        continue;
                    }
                    let event = self.user_prompt_submit(&input.text);
                    items.push(ClaimedItem::Prompt(
                        command.clone(),
                        input.text.clone(),
                        Box::new(event),
                    ));
                }
                Lane::Notice(notice) => items.push(ClaimedItem::Notice(notice.text())),
            }
        }
        let hooks = self
            .deps
            .hooks
            .has_hooks(HookEventName::UserPromptSubmit)
            .then(|| self.deps.hooks.clone());
        Some(Claimed {
            items,
            hooks,
            abort: running.abort.token().clone(),
        })
    }

    /// 结局：并入与丢弃的出队（丢弃时发提示）；留下的记下结果（回合结束后由 `start_next` 取用）。
    pub(crate) fn settle_steering(&mut self, fates: Vec<(CommandId, Fate)>) {
        for (command, fate) in fates {
            if !self.queue.iter().any(|input| input.command == command) {
                continue;
            }
            match fate {
                Fate::Absorbed => self.remove_input(&command),
                Fate::Dropped(notice) => {
                    self.remove_input(&command);
                    // 只供显示，排在本次工具边界的 queued_command 之前（先给出 notices）。
                    if let Some(text) = notice {
                        self.log.append(KernelEvent::SystemNotice {
                            level: NoticeLevel::Warning,
                            text,
                        });
                    }
                }
                Fate::Waits(outcome) => {
                    self.screened.insert(command, outcome);
                }
            }
        }
        self.emit_queue();
    }
}

/// 领取之后、交回之前的一项。
enum Screened {
    Prompt(CommandId, String, Box<HookOutcome>),
    Notice(String),
}

impl ActorSink {
    async fn claim(&self) -> Option<Claimed> {
        let (reply, answer) = oneshot::channel();
        self.sender
            .send(SinkRequest::ClaimSteering { reply })
            .await
            .ok()?;
        answer.await.ok().flatten()
    }

    /// 领取 → 逐条跑插话的 UPS → 交回结局（query.md §4.1 的表），按到达顺序给出插话与通知。
    pub(crate) async fn absorbed(&self) -> Vec<Absorbed> {
        let Some(claimed) = self.claim().await else {
            return Vec::new();
        };
        let mut fates = Vec::new();
        let mut screened = Vec::new();
        let mut barrier = false;
        for item in claimed.items {
            let (command, text, event) = match item {
                ClaimedItem::Notice(text) => {
                    screened.push(Screened::Notice(text));
                    continue;
                }
                ClaimedItem::Prompt(..) if barrier => continue,
                ClaimedItem::Prompt(command, text, event) => (command, text, event),
            };
            let outcome = match &claimed.hooks {
                Some(hooks) => hooks.run(*event, claimed.abort.clone()).await,
                None => HookOutcome::default(),
            };
            if claimed.abort.is_cancelled() {
                // 中止：这条没有可信的结果，回合结束后重跑 hook。
                fates.push((command, Fate::Waits(None)));
                break;
            }
            if outcome.blocking_error.is_some() {
                fates.push((command, Fate::Dropped(outcome.notices.first().cloned())));
                continue;
            }
            if outcome.prevent_continuation {
                // 留下，之后的插话本次也不并入（屏障）；通知不受影响。
                fates.push((command, Fate::Waits(Some(Box::new(outcome)))));
                barrier = true;
                continue;
            }
            screened.push(Screened::Prompt(command, text, Box::new(outcome)));
        }
        // 领取后回合被中止：全部留下（"leaving commands queued"），已跑完的记下结果；通知不并入，
        // 留在通道里。
        let aborted = claimed.abort.is_cancelled();
        let mut absorbed = Vec::new();
        for item in screened {
            match item {
                Screened::Prompt(command, _, outcome) if aborted => {
                    fates.push((command, Fate::Waits(Some(outcome))));
                }
                Screened::Notice(_) if aborted => {}
                Screened::Prompt(command, text, outcome) => {
                    if let Some(title) = outcome.session_title.clone() {
                        self.title(title).await;
                    }
                    fates.push((command, Fate::Absorbed));
                    absorbed.push(Absorbed::Prompt(Steered {
                        text,
                        after: outcome.messages,
                    }));
                }
                Screened::Notice(text) => absorbed.push(Absorbed::Notice(text)),
            }
        }
        let (reply, answer) = oneshot::channel();
        if self
            .sender
            .send(SinkRequest::SettleSteering { fates, reply })
            .await
            .is_ok()
        {
            let _ = answer.await;
        }
        absorbed
    }
}
