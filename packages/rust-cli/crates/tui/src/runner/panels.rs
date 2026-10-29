//! L30 面板在 runner 里的部分（docs/specs/rust-cli/tui-panels.md §0、§1）：子代理的变化通知、子代理列表与后台任务的
//! 快照、只读视图的查询。回执按请求序号丢掉过时的（`FuturesUnordered` 里回执的先后不定），不用超时。
use futures_util::stream::{self, BoxStream};
use futures_util::{FutureExt, StreamExt};
use ratatui::backend::Backend;
use rust_cli_kernel_api::ids::AgentId;
use rust_cli_kernel_api::panels::{ShellDetail, SubagentEntry, TasksSnapshot};
use rust_cli_kernel_api::session_api::{KernelError, SessionView};

use super::Loop;
use super::session::Done;
use crate::ui::agents::Viewing;
use crate::ui::i18n;
use crate::ui::panels::Panel;
use crate::view::Transcript;

/// 面板的查询状态：子代理的变化通知与各查询的最新序号。
pub(super) struct Panels {
    changes: BoxStream<'static, ()>,
    /// 子代理列表与后台任务的同一份快照的请求序号（修复线 F34；原来列表与后台任务各有一个序号）。
    tasks_seq: u64,
    view_seq: u64,
    shell_seq: u64,
}

impl Default for Panels {
    fn default() -> Self {
        Self {
            changes: stream::pending().boxed(),
            tasks_seq: 0,
            view_seq: 0,
            shell_seq: 0,
        }
    }
}

impl Panels {
    /// 下一次子代理变化；通知流结束后不再等它。
    pub(super) async fn changed(&mut self) {
        if self.changes.next().await.is_none() {
            self.changes = stream::pending().boxed();
        }
    }
}

/// 面板查询的结果。
pub(super) enum PanelDone {
    /// 子代理列表与后台任务的同一份快照（修复线 F34）。
    Tasks {
        seq: u64,
        snapshot: Result<TasksSnapshot, KernelError>,
    },
    /// 子代理的只读快照；`entry` 是发请求时列表里的这一项（名称、版本、状态）。
    Viewed {
        seq: u64,
        entry: Box<SubagentEntry>,
        view: Box<Result<SessionView, KernelError>>,
    },
    /// Shell 详情的一次读取。
    Shell {
        id: String,
        seq: u64,
        detail: Result<Option<ShellDetail>, KernelError>,
    },
}

impl<B: Backend> Loop<'_, B> {
    /// 有了会话（新建、切换）：订阅子代理的变化，先取一次子代理列表与后台任务。
    pub(super) fn attach_panels(&mut self) {
        let Some(session) = self.session.clone() else {
            return;
        };
        self.panels.changes = session.subagent_changes();
        self.refresh_tasks();
    }

    /// 离开会话：不再订阅，列表、pill 与查看都清掉（新会话的快照到达前不显示旧的）；在途的快照回执作废。
    ///
    /// 修复（修复线 F34）：原来这里只清列表，pill 留到新会话打开时 `App::reset` 才清，切换会话的这段时间里是“列表没了、
    /// pill 还在”的帧。两者是同一份快照，一起清。
    pub(super) fn detach_panels(&mut self) {
        self.panels.changes = stream::pending().boxed();
        self.panels.tasks_seq += 1;
        self.panels.view_seq += 1;
        self.app.agents = crate::ui::agents::AgentList::default();
        self.app.background.clear();
        self.app.focus = None;
        if self.app.viewing.take().is_some() {
            self.cache.clear();
            self.app.find = None;
            self.app.restore_bottom = None;
        }
    }

    /// 重取子代理列表与后台任务的同一份快照：子代理的变化通知、主会话里后台任务相关的事件、打开会话、`/tasks` 与停止的
    /// 回执之后都发这一个查询（tui-panels.md §0、§3.1）。
    ///
    /// 修复（修复线 F34，用户决定 2026-10-20）：原来列表在变化通知之后取 `subagents()`、pill 在主会话事件之后取
    /// `background_tasks()`，两次查询、两个序号，谁先画出不定，子代理里再派生的子代理也不刷新 pill（F32 查明，D16）。
    /// 现在一次查询、一个序号，回执一次收下、一次重画，列表的行与 pill 同一帧出现（两者出自同一份快照）。
    pub(super) fn refresh_tasks(&mut self) {
        let Some(session) = self.session.clone() else {
            return;
        };
        self.panels.tasks_seq += 1;
        let seq = self.panels.tasks_seq;
        self.pending.push(
            async move {
                let snapshot = session.tasks_snapshot().await;
                Done::Panel(PanelDone::Tasks { seq, snapshot })
            }
            .boxed(),
        );
    }

    pub(super) fn panel(&mut self, panel: Panel) {
        match panel {
            Panel::ViewAgent(Some(agent)) => self.load_agent(&agent),
            Panel::ViewAgent(None) => self.stop_viewing(),
            Panel::ReadShell(id) => {
                // 每次打开详情一个新序号：关掉或重开过之后，旧的回复按序号丢弃（tui-panels.md §3.3）。
                self.panels.shell_seq += 1;
                let seq = self.panels.shell_seq;
                self.app.shell_reading(&id, seq);
                self.read_shell(id, seq, false);
            }
        }
    }

    /// 读一次 Shell 详情；`later` 为真时 1 s 后再读（还在跑时每秒刷新，同一时刻只有一个读取）。
    fn read_shell(&mut self, id: String, seq: u64, later: bool) {
        let Some(session) = self.session.clone() else {
            return;
        };
        self.pending.push(
            async move {
                if later {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
                let detail = session.shell_detail(&id).await;
                Done::Panel(PanelDone::Shell { id, seq, detail })
            }
            .boxed(),
        );
    }

    /// 取子代理的只读快照（打开查看、或它的版本与状态变了）。
    fn load_agent(&mut self, agent: &AgentId) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let Some(entry) = self
            .app
            .agents
            .entries
            .iter()
            .find(|entry| &entry.agent == agent)
            .cloned()
        else {
            return;
        };
        self.panels.view_seq += 1;
        let seq = self.panels.view_seq;
        self.pending.push(
            async move {
                let view = session.subagent_view(&entry.agent).await;
                Done::Panel(PanelDone::Viewed {
                    seq,
                    entry: Box::new(entry),
                    view: Box::new(view),
                })
            }
            .boxed(),
        );
    }

    /// 回到主会话：两个视图的行 id 会重复，清掉排版缓存、滚动、选区与单独展开的块。footer 的命中率换回主会话的：清空
    /// 回执（序号加一，在途的子会话回执作废）再查主会话（修复线 F112，tui-panels.md §1.7）。
    fn stop_viewing(&mut self) {
        self.panels.view_seq += 1;
        if self.app.viewing.take().is_some() {
            self.reset_transcript_view();
            self.app.request_stats.clear();
            self.refresh_stats();
        }
    }

    /// 显示的 transcript 换了（开始查看、换一个、返回主会话）：滚动、选区、单独展开与搜索模式都属于原来那份（搜索：修复线
    /// F114，tui-input.md §2.3.4）。
    fn reset_transcript_view(&mut self) {
        self.cache.clear();
        self.app.scroll = 0;
        self.app.new_activity = false;
        self.app.selection = None;
        self.app.expanded_cells.clear();
        self.app.find = None;
        self.app.restore_bottom = None;
    }

    pub(super) fn on_panel(&mut self, done: PanelDone) {
        let lang = self.app.lang;
        match done {
            PanelDone::Tasks { seq, snapshot } => {
                if seq != self.panels.tasks_seq {
                    return;
                }
                // 会话不支持时当作没有子代理、没有后台任务。列表与 pill 一起换（同一份快照）。
                let TasksSnapshot {
                    subagents,
                    background,
                } = snapshot.unwrap_or_default();
                self.app.agents = crate::ui::agents::AgentList {
                    entries: subagents,
                    at: Some(std::time::Instant::now()),
                };
                self.app.background = background;
                // pill 没了：选中也随之取消（tui-panels.md §3.1）。
                let gone = crate::ui::manage::pill(&self.app.background, lang).is_none();
                if gone && self.app.focus == Some(crate::ui::agents::Focus::Pill) {
                    self.app.focus = None;
                }
                self.follow_viewed();
            }
            PanelDone::Viewed { seq, entry, view } => {
                if seq != self.panels.view_seq {
                    return;
                }
                match *view {
                    Ok(view) => {
                        let ids: Vec<_> = view.messages.iter().map(|m| m.id().clone()).collect();
                        let now = self.now_ms();
                        // 同一个子代理的新快照只多了消息：折进已有的视图，行 id 与排版缓存保留（修复线 F6，审查
                        // R5-11；以前每次提交都整体重建、清掉排版缓存，所有块重排）。
                        if let Some(viewing) = self.app.viewing.as_mut()
                            && viewing.agent == entry.agent
                            && ids.starts_with(&viewing.messages)
                            && viewing.view.append_view(&view, viewing.messages.len(), now)
                        {
                            viewing.version = entry.version;
                            viewing.status = entry.status;
                            viewing.messages = ids;
                            // 同一个子代理有了新提交（可能多了用量行）：重查它的命中率（F112）。
                            self.refresh_stats();
                            return;
                        }
                        let fresh = self
                            .app
                            .viewing
                            .as_ref()
                            .is_none_or(|viewing| viewing.agent != entry.agent);
                        self.app.viewing = Some(Viewing {
                            agent: entry.agent,
                            kind: entry.agent_type,
                            description: entry.description,
                            version: entry.version,
                            status: entry.status,
                            messages: ids,
                            view: Transcript::from_view(view, now),
                            owner: entry.owner,
                        });
                        if fresh {
                            self.reset_transcript_view();
                            // 查看对象换了（F112）：命中率的回执清空、在途的作废，再查新对象的子会话。
                            self.app.request_stats.clear();
                        } else {
                            self.cache.clear();
                        }
                        self.refresh_stats();
                    }
                    Err(error) => self.app.error(i18n::rejected(lang, &error.to_string())),
                }
            }
            PanelDone::Shell { id, seq, detail } => {
                if seq != self.panels.shell_seq {
                    return;
                }
                // 读不到时当作内核已不认识它（详情保留最后一次的结果）。
                let again = self
                    .app
                    .on_shell_detail(&id, seq, detail.unwrap_or_default());
                if again {
                    self.read_shell(id, seq, true);
                }
            }
        }
    }

    /// 新的列表快照：正在查看的子代理不在了就改看它的 owner（还在、没驱逐时），否则回到主会话（tui-panels.md §1.6）；
    /// 版本或状态变了就重取它的快照。
    fn follow_viewed(&mut self) {
        match self.app.viewed_gone(std::time::Instant::now()) {
            Some(Some(owner)) => return self.load_agent(&owner),
            Some(None) => return self.stop_viewing(),
            None => {}
        }
        let Some(viewing) = &self.app.viewing else {
            return;
        };
        let changed = self
            .app
            .agents
            .entries
            .iter()
            .find(|entry| entry.agent == viewing.agent)
            .filter(|entry| entry.version != viewing.version || entry.status != viewing.status)
            .map(|entry| entry.agent.clone());
        if let Some(agent) = changed {
            self.load_agent(&agent);
        }
    }
}
