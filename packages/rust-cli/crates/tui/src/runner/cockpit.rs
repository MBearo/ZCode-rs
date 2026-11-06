//! 驾驶舱在 runner 里的部分（docs/specs/rust-cli/tui-cockpit.md §1）：监听一直开着，页开着时重读页、关着时重读槽的数（§3.4）；
//! 现在的会话的现场与笔记由这里写；窗格、最后的回复、停止、去窗格问内核或入口。回执按序号丢掉过时的，不用超时。
use futures_util::FutureExt;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::peers::{PeerAddress, PeerPresence, TmuxPlace};
use rust_cli_kernel_api::session_api::{Command, KernelEvent, TurnOutcome};
use rust_cli_kernel_api::store::{SessionOrder, SessionQuery};

use super::Loop;
use super::projects::ProjectsDone;
use super::session::Done;
use crate::services::{StoreFile, Stores};

/// 一次读最后的回复时的并发（§2.3）。
const REPLIES_AT_ONCE: usize = 8;

pub(super) struct CockpitState {
    /// 登记处与笔记的变化：第一轮循环取一次，一直开着（§1）；没有端口或监听建不起来时永远不出项。
    pub(super) changes: BoxStream<'static, ()>,
    watching: bool,
    /// 上一轮循环页开没开着：关上时重读一次槽的数（开着时变化给了页）。
    was_open: bool,
    /// 槽的数（§3.4）：同一时刻至多一次在读；读着时又有变化，回执到了再读一次。
    news_loading: bool,
    news_again: bool,
    /// 终端有没有焦点（没有焦点报告时一直算有）。
    pub(super) focused: bool,
    /// 正在做的一行与它在哪台机器上。
    now: Option<(String, Option<String>)>,
    /// 最后写进登记的现场的内容（[`content`]：不含 `at_ms` 与在跑的子代理的用时）。
    written: PeerPresence,
    /// 现场写给了哪个会话：换了会话时登记里的已经清掉，要重写。
    written_for: Option<String>,
    pub(super) peek_seq: u64,
}

impl Default for CockpitState {
    fn default() -> Self {
        Self {
            changes: stream::pending().boxed(),
            watching: false,
            was_open: false,
            news_loading: false,
            news_again: false,
            focused: true,
            now: None,
            written: PeerPresence::default(),
            written_for: None,
            peek_seq: 0,
        }
    }
}

/// 比较用的内容（tui-cockpit.md §4）：在跑的子代理的用时随时间走，不是内容，记成 0。
///
/// 修复（2026-11-05）：以前直接比较 `presence_of` 的结果，里面有在跑的子代理的 `elapsed_ms`；每份快照（子会话每次提交、
/// 每次活动变化都重取）的用时都不同，所以有子代理在跑时每轮循环都重写 `<peers>/<pid>.json`，实测每秒约 4 次，每个监听
/// `<peers>` 的 TUI 跟着重读 `project_sessions` 与笔记：写的进程与一个空闲的 TUI 各占约 150% CPU、200 多个线程。读的人
/// 本来就按 `elapsedMs + now − atMs` 外推（`cockpit_pane.rs`），在跑时不用重写；`running` 变了（结束、停放、续跑）照常写。
fn content(presence: &PeerPresence) -> PeerPresence {
    let mut content = presence.clone();
    for agent in content.agents.iter_mut().filter(|agent| agent.running) {
        agent.elapsed_ms = 0;
    }
    content
}

/// `puffin-tui/cockpit.json` 里记着的分组。
pub(super) async fn remembered(stores: Option<&dyn Stores>) -> bool {
    let Some(stores) = stores else {
        return false;
    };
    let Ok(Some(text)) = stores.read(StoreFile::Cockpit).await else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|value| value["byFolder"].as_bool())
        .unwrap_or(false)
}

impl Loop<'_> {
    /// 每轮循环问一次：第一次取监听（建好时先出一项，槽的数从它读起）；页关上了就重读槽的数；选中的代理的窗格要不要再读；
    /// 现场变了就写。
    pub(super) fn cockpit_sync(&mut self) {
        if !self.cockpit.watching {
            self.cockpit.watching = true;
            if let Some(port) = &self.options.services.cockpit {
                self.cockpit.changes = port.changes();
            }
        }
        let open = self.app.cockpit_open();
        if std::mem::replace(&mut self.cockpit.was_open, open) && !open {
            self.cockpit_news();
        }
        if let Some(id) = self.app.cockpit_peek_wanted() {
            self.cockpit_peek(id);
        }
        self.publish_presence();
    }

    pub(super) fn cockpit_changed(&mut self, change: Option<()>) {
        match change {
            Some(()) if self.app.cockpit_open() => self.refresh_sessions(),
            Some(()) => self.cockpit_news(),
            // 监听建不起来：只有打开时读的那一次与本进程的事件（§1）。
            None => self.cockpit.changes = stream::pending().boxed(),
        }
    }

    /// 槽的数（§3.4）：同页的查询（所有文件夹）与笔记，按 `kind_from` 分组，只留不是 Rest 的。读着时来的变化不另起一次，
    /// 记下来等回执（[`Self::cockpit_news_done`]）：同一时刻至多一次在读，最后的变化也读到。
    fn cockpit_news(&mut self) {
        let Some(port) = self.options.services.cockpit.clone() else {
            return;
        };
        if self.cockpit.news_loading {
            self.cockpit.news_again = true;
            return;
        }
        self.cockpit.news_loading = true;
        let kernel = self.kernel.clone();
        let query = SessionQuery {
            environment: self.app.view.workspace().environment.clone(),
            root: None,
            order: SessionOrder::default(),
            limit: crate::ui::projects::LIMIT,
        };
        self.pending.push(
            async move {
                let Ok(found) = kernel.project_sessions(query).await else {
                    return Done::Projects(ProjectsDone::News(None));
                };
                let ids = found.sessions.iter().map(|s| s.record.id.clone()).collect();
                let notes = port.notes(ids).await;
                let kinds = found
                    .sessions
                    .iter()
                    .map(|s| {
                        let note = notes.get(&s.record.id).cloned().unwrap_or_default();
                        let kind = crate::ui::cockpit::kind_from(s.activity, &note);
                        (s.record.id.as_str().to_owned(), kind)
                    })
                    .filter(|(_, kind)| *kind != crate::ui::cockpit::Kind::Rest)
                    .collect();
                Done::Projects(ProjectsDone::News(Some(kinds)))
            }
            .boxed(),
        );
    }

    /// 槽的数到了：读失败时留着上一次的（界面不另说，§1）；读着时有过变化就再读一次。
    pub(super) fn cockpit_news_done(
        &mut self,
        kinds: Option<Vec<(String, crate::ui::cockpit::Kind)>>,
    ) {
        self.cockpit.news_loading = false;
        if let Some(kinds) = kinds {
            self.app.cockpit_news = kinds;
        }
        if std::mem::take(&mut self.cockpit.news_again) {
            self.cockpit_news();
        }
    }

    /// 现场（§4）：内容变了才写；`at_ms` 是写的时刻，读的人从它外推在跑的子代理的用时。
    fn publish_presence(&mut self) {
        let Some(session) = self.app.session.clone() else {
            return;
        };
        let mut presence = crate::ui::cockpit::presence_of(&self.app, self.cockpit.now.as_ref());
        let written = content(&presence);
        let fresh = self.cockpit.written_for.as_ref() != Some(&session);
        if written == self.cockpit.written && (!fresh || written == PeerPresence::default()) {
            self.cockpit.written_for = Some(session);
            return;
        }
        self.cockpit.written = written;
        self.cockpit.written_for = Some(session.clone());
        let now = self.now_ms();
        for agent in &mut presence.agents {
            agent.at_ms = now;
        }
        let kernel = self.kernel.clone();
        self.pending.push(
            async move {
                kernel
                    .set_presence(&SessionId::new(session), presence)
                    .await;
                Done::Projects(ProjectsDone::Accepted)
            }
            .boxed(),
        );
    }

    /// 事件在进视图之前（`apply` 拿走它）：正在做的一行、回合结束的笔记（§1.2）。
    pub(super) fn cockpit_event(&mut self, event: &KernelEvent, wall: u64) {
        match event {
            KernelEvent::ToolStarted { name, input, .. } => {
                let summary = crate::ui::interaction_ask::summary_of(name, input);
                let name = crate::ui::interaction_ask::display_name(name);
                let line = format!("{name}  {summary}").trim_end().to_owned();
                let machine = input["machine"].as_str().map(str::to_owned);
                self.cockpit.now = Some((line, machine));
            }
            KernelEvent::TurnFinished { outcome, .. } => {
                self.cockpit.now = None;
                let failed = match outcome {
                    TurnOutcome::Failed { message } => Some(message.clone()),
                    _ => None,
                };
                self.turn_note(wall, failed);
            }
            _ => {}
        }
    }

    fn turn_note(&mut self, at: u64, failed: Option<String>) {
        let Some(port) = self.options.services.cockpit.clone() else {
            return;
        };
        let Some(id) = self.app.session.clone() else {
            return;
        };
        // 看着它结束的不算新（同一个时刻：`endedAt > seenAt` 为假）。
        let watched = self.cockpit.focused && !self.app.cockpit_open();
        self.pending.push(
            async move {
                let id = SessionId::new(id);
                port.write_turn(id.clone(), at, failed).await;
                if watched {
                    port.mark_seen(id, at).await;
                }
                Done::Projects(ProjectsDone::Accepted)
            }
            .boxed(),
        );
    }

    /// 终端的焦点：得到焦点、页没开着时现在的会话算看过（§1.2）。
    pub(super) fn cockpit_focus(&mut self, focused: bool) {
        self.cockpit.focused = focused;
        if focused
            && !self.app.cockpit_open()
            && let Some(id) = self.app.session.clone()
        {
            self.mark_seen(id);
        }
    }

    pub(super) fn mark_seen(&mut self, id: String) {
        let Some(port) = self.options.services.cockpit.clone() else {
            return;
        };
        let now = self.now_ms();
        self.pending.push(
            async move {
                port.mark_seen(SessionId::new(id), now).await;
                Done::Projects(ProjectsDone::Accepted)
            }
            .boxed(),
        );
    }

    fn cockpit_peek(&mut self, id: String) {
        self.cockpit.peek_seq += 1;
        let seq = self.cockpit.peek_seq;
        let kernel = self.kernel.clone();
        self.pending.push(
            async move {
                let view = kernel.session_snapshot(&SessionId::new(id.as_str())).await;
                let result = view.ok().map(|view| crate::ui::cockpit::peek_of(id, &view));
                Done::Projects(ProjectsDone::Peek { seq, result })
            }
            .boxed(),
        );
    }

    pub(super) fn cockpit_load_replies(&mut self, ids: Vec<String>) {
        let kernel = self.kernel.clone();
        self.pending.push(
            async move {
                let replies = stream::iter(ids)
                    .map(|id| {
                        let kernel = kernel.clone();
                        async move {
                            let view = kernel.session_snapshot(&SessionId::new(id.as_str())).await;
                            let reply = crate::ui::cockpit::peek_of(id.clone(), &view.ok()?).reply;
                            (!reply.is_empty()).then_some((id, reply))
                        }
                    })
                    .buffer_unordered(REPLIES_AT_ONCE)
                    .filter_map(std::future::ready)
                    .collect::<Vec<_>>()
                    .await;
                Done::Projects(ProjectsDone::Replies(replies))
            }
            .boxed(),
        );
    }

    /// ⌃x（§1.3）：现在的会话同 esc；别的进程经它的收件箱。
    pub(super) fn cockpit_stop(&mut self, address: Option<PeerAddress>) {
        let Some(address) = address else {
            self.send(super::session::Sent::Interrupt, Command::Interrupt);
            return;
        };
        let kernel = self.kernel.clone();
        self.pending.push(
            async move {
                match kernel.stop_peer(&address).await {
                    Ok(()) => Done::Projects(ProjectsDone::Accepted),
                    Err(error) => Done::Projects(ProjectsDone::CockpitFailed(error.to_string())),
                }
            }
            .boxed(),
        );
    }

    pub(super) fn cockpit_go_to(&mut self, place: TmuxPlace) {
        let Some(port) = self.options.services.cockpit.clone() else {
            return;
        };
        self.pending.push(
            async move {
                match port.go_to(place).await {
                    Ok(()) => Done::Projects(ProjectsDone::Accepted),
                    Err(error) => Done::Projects(ProjectsDone::CockpitFailed(error)),
                }
            }
            .boxed(),
        );
    }

    pub(super) fn cockpit_remember(&mut self, by_folder: bool) {
        let Some(stores) = self.options.services.stores.clone() else {
            return;
        };
        let text = format!("{{\"byFolder\":{by_folder}}}\n");
        self.pending.push(
            async move {
                let _ = stores.write(StoreFile::Cockpit, text).await;
                Done::Projects(ProjectsDone::Accepted)
            }
            .boxed(),
        );
    }
}
