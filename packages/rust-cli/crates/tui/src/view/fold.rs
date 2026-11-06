//! 事件 → 行（docs/specs/rust-cli/tui.md §4.2）。冷加载（[`SessionView`]）与实时事件走同一套处理。
use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::message::text_of;
use rust_cli_kernel_api::model::BlockDelta;
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::permission::ExecutionState;
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent, SessionView, TurnOutcome};
use serde_json::Value;

use rust_cli_tui_kit::app::widgets::entries::Compaction;

use super::{Body, Compact, Fire, Row, Segment, ToolRow, ToolStatus, Transcript, TurnState};

#[path = "fold_message.rs"]
mod message;

/// 运行中预览保留的输出行数。
const PREVIEW_LINES: usize = 5;

impl Transcript {
    /// 从会话的当前视图重建（打开会话、`HistoryRewritten` 之后）。
    pub fn from_view(view: SessionView, now_ms: u64) -> Self {
        let execution = ExecutionState::from_parts(view.mode, view.plan);
        let mut transcript = Self::new(view.selection, execution, view.meta.workspace);
        transcript.title = view.meta.title;
        // 子代理的会话也有 parent：只有 fork 出来的会话画痕迹行（Puffin `load`）。
        if view.meta.parent.is_some()
            && view.meta.kind != rust_cli_kernel_api::store::SessionKind::Subagent
        {
            transcript.push(Body::Forked { title: None }, now_ms);
        }
        // 冷加载的上下文提示与用量来自快照（F27：以前要等下一条回复）。
        transcript.context = view.context_notice;
        transcript.occupancy = view.context_usage;
        transcript.speed = view.output_speed;
        for message in view.messages {
            transcript.message(message, now_ms);
        }
        transcript.returned.clear();
        transcript.settle(None);
        // 快照里的 `!` 命令都跑完了（被中断的没有输出消息）。
        transcript.shell_open = false;
        // 冷加载的回合数是用户消息的条数（Puffin `from_view`）。
        let users = transcript
            .rows
            .iter()
            .filter(|row| matches!(row.body, Body::User { .. }))
            .count();
        transcript.extras.turns.0 = u32::try_from(users).unwrap_or(u32::MAX);
        transcript.pending = view.pending;
        for (_, request) in &transcript.pending.clone() {
            transcript.mark_approval(request, true);
        }
        transcript.queue = view.queue;
        transcript.peer_held = view.peer_held;
        transcript.last_seq = view.last_seq;
        // 快照时有回合在跑：补一行运行中，之后的 TurnFinished 收尾它。
        if view.running {
            transcript.push(Body::Turn(TurnState::Running), now_ms);
        }
        transcript.cold = transcript.next_id;
        transcript
    }

    /// 只读查看的刷新（tui-panels.md §1.4，修复线 F6，审查 R5-11）：新快照只比上一份多了 `messages`、其余视图字段都
    /// 没变时，只把它们折进来，接在 [`Self::from_view`] 建好的行后面（运行中的回合行照旧排在最后）。行 id 与版本延续，
    /// 没变的块不重排；结果与对整份快照 `from_view` 相同。以前每次提交都整体重建、清掉排版缓存，所有块重排。
    /// 视图字段变了时返回 false（调用方整体替换）。
    pub fn append_view(&mut self, view: &SessionView, folded: usize, now_ms: u64) -> bool {
        let same = view.meta.title == self.title
            && view.meta.workspace == self.workspace
            && view.selection == self.selection
            && ExecutionState::from_parts(view.mode, view.plan) == self.execution
            && view.pending.is_empty()
            && self.pending.is_empty()
            && view.queue == self.queue
            && view.peer_held == self.peer_held
            && view.last_seq == self.last_seq
            && view.messages.len() >= folded;
        if !same {
            return false;
        }
        // 子代理的快照带它的 ctx、上下文提示与最近一个请求的速度（修复线 F112，tui-panels.md §1.7）：它们随每次提交变，
        // 不算“视图字段变了”，取新快照的值（修前追加只折消息，三项停在第一份快照的值）。
        self.occupancy = view.context_usage;
        self.context = view.context_notice;
        self.speed = view.output_speed;
        if matches!(
            self.rows.last(),
            Some(Row {
                body: Body::Turn(TurnState::Running),
                ..
            })
        ) {
            self.rows.pop();
        }
        for message in &view.messages[folded..] {
            self.message(message.clone(), now_ms);
        }
        self.returned.clear();
        self.settle(None);
        if view.running {
            self.push(Body::Turn(TurnState::Running), now_ms);
        }
        self.cold = self.next_id;
        true
    }

    /// 折叠一个事件。seq 不大于已处理的事件是重新订阅时的补发，忽略。
    pub fn apply(&mut self, event: SequencedEvent, now_ms: u64) {
        if event.seq <= self.last_seq {
            return;
        }
        self.last_seq = event.seq;
        // 内核认领了发出的输入（`in_flight.rs`）：与它的队列、它的回合在同一次折叠里交接。
        self.in_flight.claim(&event.event);
        match event.event {
            KernelEvent::TurnStarted { .. } => {
                self.settle(None);
                self.streamed_chars = 0;
                self.live_chars = 0;
                self.live_wide = 0;
                self.notice = None;
                self.retried = None;
                self.turn_user = None;
                self.push(Body::Turn(TurnState::Running), now_ms);
            }
            KernelEvent::TurnFinished { outcome, .. } => {
                self.notice = None;
                // 被中断的 `!` 命令没有输出消息：回合结束时它也不在跑了。
                self.shell_open = false;
                // `/stats` 的回合数与时长（Puffin `turn_finished`）：从这一回合的行出现时算起。
                let started = self.rows.iter().rev().find_map(|row| match row.body {
                    Body::Turn(TurnState::Running) => Some(row.created_ms),
                    _ => None,
                });
                let (turns, active) = &mut self.extras.turns;
                *turns = turns.saturating_add(1);
                if let Some(started) = started {
                    *active += now_ms.saturating_sub(started);
                }
                self.finish(outcome, now_ms);
            }
            // 流增量与工具开始说明请求已经往前走了：清掉重试或停顿通知（stall-visibility.md §3.2）。
            KernelEvent::StreamDelta {
                response,
                index,
                delta,
                ..
            } => {
                self.notice = None;
                self.delta(response, index, delta, now_ms);
            }
            KernelEvent::MessageDone { message } => {
                self.message(message, now_ms);
                for title in std::mem::take(&mut self.returned) {
                    self.push(Body::Returned { title }, now_ms);
                }
            }
            KernelEvent::ToolStarted {
                tool_use,
                name,
                input,
            } => {
                self.notice = None;
                self.tool_started(tool_use, name, input, now_ms);
            }
            KernelEvent::ToolProgress { tool_use, progress } => {
                if self.agent_progress(&tool_use, &progress.data, now_ms) {
                    return;
                }
                if let Some(output) = progress.data["output"].as_str() {
                    let lines: Vec<&str> = output.trim_end().lines().collect();
                    let preview = lines[lines.len().saturating_sub(PREVIEW_LINES)..].join("\n");
                    self.update_tool(&tool_use, |tool| tool.preview = preview);
                }
            }
            KernelEvent::ToolDone { tool_use, output } => {
                let status = if output.is_error {
                    ToolStatus::Error
                } else {
                    ToolStatus::Success
                };
                let text = text_of(&output.content);
                // 修 bug：子代理的数字按“Agent 调用 → 子会话”的关联查（`subagents`）。以前只在 `KernelEvent::Subagent`
                // 到达时记下，可内核从来不发这个事件（feat/rust-cli 已把它删掉），关联一直是空的：结束的子代理不显示它报告
                // 的 token，transcript 的 Agent 行没有数字。现在从 Agent 工具结果的 display 取子会话（kernel-api
                // `agents::DISPLAY_KEY` 的 `childSessionId`，同内核落库的 `SubagentLinked`）：后台的启动时就有，前台的结束时有。
                if let Some(child) =
                    output.display[rust_cli_kernel_api::agents::DISPLAY_KEY]["childSessionId"]
                        .as_str()
                        .filter(|child| !child.is_empty())
                {
                    self.subagents.insert(
                        tool_use.to_string(),
                        rust_cli_kernel_api::ids::SessionId::new(child),
                    );
                }
                self.update_tool(&tool_use, |tool| {
                    tool.status = status;
                    tool.output = text;
                    tool.display = output.display;
                    if tool.started_ms > 0 {
                        tool.ended_ms = now_ms;
                    }
                });
            }
            KernelEvent::InteractionAsked {
                interaction,
                request,
            } => {
                self.mark_approval(&request, true);
                self.pending.push((interaction, request));
            }
            KernelEvent::InteractionResolved { interaction } => {
                if let Some(at) = self.pending.iter().position(|(id, _)| *id == interaction) {
                    let (_, request) = self.pending.remove(at);
                    self.mark_approval(&request, false);
                }
            }
            KernelEvent::QueueChanged { items } => self.queue = items,
            KernelEvent::ModelChanged { selection } => self.selection = selection,
            // 修复线 F20：`ModeChanged` 只带权限模式；旧的单值 `Plan` 按“打开 plan”折叠。
            KernelEvent::ModeChanged { mode } => self.execution = self.execution.with_mode(mode),
            KernelEvent::PlanChanged { enabled } => {
                self.execution = self.execution.with_plan(enabled)
            }
            KernelEvent::TitleChanged { title } => self.title = title,
            KernelEvent::ContextNotice { notice } => self.context = notice,
            // footer 的 ctx（F27）：F14 到 F27 之间内核不发这个事件。
            KernelEvent::ContextUsage {
                used_tokens,
                max_tokens,
                compact_at,
            } => {
                self.occupancy = Some(rust_cli_kernel_api::compaction::ContextOccupancy {
                    used_tokens,
                    max_tokens,
                    compact_at,
                });
                if let Some((id, true)) = self.compact {
                    self.compact = None;
                    self.edit_compact(id, |compact| {
                        compact.status = Compaction::Success;
                        compact.after = Some(used_tokens);
                    });
                }
            }
            // footer 的 TPS（F105）：只取最新的一份。
            KernelEvent::OutputSpeed { speed } => self.speed = Some(speed),
            // 压缩是一行（Puffin `compact_started` … `context_usage`）：开始时在跑，边界消息给压缩前的 token，做完之后的第一次
            // 上下文占用给压缩后的 token 并定为成功。以前开始与做完各是一行标记。
            KernelEvent::CompactStarted { .. } => {
                let at = self.push(
                    Body::Compact(Compact {
                        status: Compaction::Running,
                        before: None,
                        after: None,
                    }),
                    now_ms,
                );
                self.compact = Some((self.rows[at].id, false));
            }
            KernelEvent::CompactDone { .. } => {
                if let Some((_, done)) = self.compact.as_mut() {
                    *done = true;
                }
            }
            // 开始了的压缩失败了（compact.md §4.1）：压缩的行定为失败（Puffin 不写原因）。以前接一行红色的原因。
            KernelEvent::CompactFailed { .. } => {
                if let Some((id, _)) = self.compact.take() {
                    self.edit_compact(id, |compact| compact.status = Compaction::Failed);
                }
            }
            // 任务清单与每回合的改动只进投影，不加行（tui-display.md §2.2、§2.4）。
            KernelEvent::TasksChanged { tasks } => self.tasks_changed(tasks),
            KernelEvent::FileChanges { turn, files, .. } => self.file_changes(turn, files),
            // HistoryRewritten 由 runner 处理（丢弃视图后重新 view()）；后台任务的快照由 runner 在
            // BackgroundWork 之后重取（tui-display.md §1）；其余不显示（tui.md §9）。
            KernelEvent::WorkspaceChanged { workspace } => self.workspace = workspace,
            KernelEvent::HistoryRewritten { .. } | KernelEvent::SessionCreated { .. } => {}
            // 终态只进投影（Shell 详情在内核已不认识这条命令时用，tui-panels.md §3.3）。
            KernelEvent::BackgroundWork { work, status } => self.background_ended(work, status),
            KernelEvent::ScheduledFire { at, kind, fold } => {
                self.fire(Fire { at, kind, fold }, now_ms);
            }
            // 只在实时事件里（不落库，冷加载没有，tui.md §4.2）。
            KernelEvent::SystemNotice { level, text } => {
                self.push(Body::Notice { level, text }, now_ms);
            }
            KernelEvent::ModelNotice { notice } => {
                if let rust_cli_kernel_api::model::ModelNotice::Retrying {
                    attempt, error, ..
                } = &notice
                {
                    self.retrying(*attempt, error);
                }
                super::Notice::fold(&mut self.notice, notice, now_ms);
            }
            // 扣住的同伴消息不进记录：批准框只看会话视图与这个快照（cross-session.md §2.6）。
            KernelEvent::PeerHeldChanged { held } => self.peer_held = held,
            // 回合结束那一行（tui-display.md §2.7，F118）：排在这个回合的错误行之后（`TurnFinished` 先到）。
            KernelEvent::TurnDuration(line) => {
                self.push(Body::Duration(Box::new(line)), now_ms);
            }
        }
    }

    pub(super) fn push(&mut self, body: Body, now_ms: u64) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        self.rows.push(Row {
            id,
            version: 0,
            created_ms: now_ms,
            body,
        });
        self.rows.len() - 1
    }

    /// 改一行并让它的版本加一（排版缓存失效）。
    fn edit(&mut self, at: usize, change: impl FnOnce(&mut Body)) {
        if let Some(row) = self.rows.get_mut(at) {
            change(&mut row.body);
            row.version += 1;
        }
    }

    fn find_tool(&self, id: &ToolUseId) -> Option<usize> {
        self.rows
            .iter()
            .rposition(|row| row.tool().is_some_and(|tool| tool.id == *id))
    }

    pub(super) fn update_tool(&mut self, id: &ToolUseId, change: impl FnOnce(&mut ToolRow)) {
        if let Some(at) = self.find_tool(id) {
            self.edit(at, |body| {
                if let Body::Tool(tool) = body {
                    change(tool);
                }
            });
        }
    }

    /// 权限询问对应的工具行显示为等待审批；回答后回到运行中。
    fn mark_approval(&mut self, request: &InteractionRequest, asked: bool) {
        let InteractionRequest::Permission { tool_use, .. } = request else {
            return;
        };
        self.update_tool(tool_use, |tool| {
            tool.status = match (asked, tool.status) {
                (true, status) if status.is_live() => ToolStatus::Approval,
                (false, ToolStatus::Approval) => ToolStatus::Running,
                (_, status) => status,
            };
        });
    }

    fn delta(&mut self, response: u32, index: usize, delta: BlockDelta, now_ms: u64) {
        let (text, reasoning) = match delta {
            BlockDelta::Text(text) => (text, false),
            BlockDelta::Thinking(text) => (text, true),
            // 工具输入在 ToolStarted 时完整给出，这里只计现场速度的字数（Puffin 同）；签名不显示。
            BlockDelta::InputJson(text) => {
                self.count_live(&text);
                return;
            }
            BlockDelta::Signature(_) => return,
        };
        if self
            .segment
            .as_ref()
            .is_some_and(|segment| segment.response.is_some_and(|r| r != response))
        {
            // 上一段没有等到 MessageDone：定稿后另开一段。
            self.settle(None);
        }
        let start = self.rows.len();
        if self.segment.is_none() {
            self.segments += 1;
        }
        let key = format!("s{}", self.segments);
        let segment = self.segment.get_or_insert_with(|| Segment {
            key: key.clone(),
            response: Some(response),
            start,
            blocks: Vec::new(),
        });
        // 工具开始时开的段（还没有流）：第一个增量给它序号（Puffin 在这里才有段，之前的调用没有 `response`）。
        if segment.key.is_empty() {
            segment.key = key;
        }
        segment.response = Some(response);
        let existing = segment
            .blocks
            .iter()
            .find(|(block, _)| *block == index)
            .map(|(_, at)| *at);
        self.streamed_chars += text.chars().count();
        self.count_live(&text);
        match existing {
            Some(at) => self.edit(at, |body| match body {
                Body::Agent { text: shown, .. } | Body::Reasoning { text: shown, .. } => {
                    shown.push_str(&text);
                }
                _ => {}
            }),
            None => {
                let body = if reasoning {
                    Body::Reasoning {
                        text,
                        streaming: true,
                        duration_ms: None,
                    }
                } else {
                    Body::Agent {
                        text,
                        streaming: true,
                    }
                };
                let at = self.push(body, now_ms);
                if let Some(segment) = self.segment.as_mut() {
                    segment.blocks.push((index, at));
                }
            }
        }
    }

    fn tool_started(&mut self, id: ToolUseId, name: String, input: Value, now_ms: u64) {
        if self.find_tool(&id).is_some() {
            self.update_tool(&id, |tool| {
                tool.status = ToolStatus::Running;
                tool.input = input;
                tool.started_ms = now_ms;
            });
            return;
        }
        let start = self.rows.len();
        let segment = self.segment.get_or_insert_with(|| Segment {
            key: String::new(),
            response: None,
            start,
            blocks: Vec::new(),
        });
        let mut tool = ToolRow::new(id, name, input, ToolStatus::Running);
        tool.response = segment.key.clone();
        tool.started_ms = now_ms;
        self.push(Body::Tool(Box::new(tool)), now_ms);
    }

    /// 定稿：流式行不再流式；`tools` 不为空时把还在运行的工具行改成它（开始过的在 `now_ms` 结束）。
    fn settle(&mut self, tools: Option<(ToolStatus, u64)>) {
        self.segment = None;
        for at in 0..self.rows.len() {
            let live = match &self.rows[at].body {
                Body::Agent { streaming, .. } | Body::Reasoning { streaming, .. } => *streaming,
                Body::Tool(tool) => tools.is_some() && tool.status.is_live(),
                _ => false,
            };
            if live {
                self.edit(at, |body| match body {
                    Body::Agent { streaming, .. } | Body::Reasoning { streaming, .. } => {
                        *streaming = false;
                    }
                    Body::Tool(tool) => {
                        if let Some((status, now_ms)) = tools {
                            tool.status = status;
                            if tool.started_ms > 0 {
                                tool.ended_ms = now_ms;
                            }
                        }
                    }
                    _ => {}
                });
            }
        }
    }

    /// 改压缩的行 `id`。
    pub(super) fn edit_compact(&mut self, id: u64, f: impl FnOnce(&mut Compact)) {
        if let Some(at) = self.rows.iter().rposition(|row| row.id == id) {
            self.edit(at, |body| {
                if let Body::Compact(compact) = body {
                    f(compact);
                }
            });
        }
    }

    fn finish(&mut self, outcome: TurnOutcome, now_ms: u64) {
        self.settle(Some((ToolStatus::Cancelled, now_ms)));
        // 回合结束时还在跑的压缩定稿（Puffin `settle_all`）：做完了的成功，停下的取消，失败的回合失败。
        if let Some((id, done)) = self.compact.take() {
            let status = match &outcome {
                _ if done => Compaction::Success,
                TurnOutcome::Success
                | TurnOutcome::HookStopped
                | TurnOutcome::StopHookPrevented => Compaction::Success,
                TurnOutcome::Interrupted => Compaction::Cancelled,
                TurnOutcome::Failed { .. } => Compaction::Failed,
            };
            self.edit_compact(id, |compact| compact.status = status);
        }
        let (state, error) = match outcome {
            // 工具的 hook 要求停止、Stop hook 阻止继续都不是失败，按成功结束显示（query.md §3，工作线 F131、F134）。
            TurnOutcome::Success | TurnOutcome::HookStopped | TurnOutcome::StopHookPrevented => {
                (TurnState::Success, None)
            }
            TurnOutcome::Interrupted => (TurnState::Interrupted, None),
            TurnOutcome::Failed { message } => (TurnState::Failed, Some(message)),
        };
        if let Some(at) = self
            .rows
            .iter()
            .rposition(|row| matches!(row.body, Body::Turn(_)))
        {
            self.edit(at, |body| *body = Body::Turn(state));
        }
        if let Some(message) = error {
            self.failed(message, now_ms);
        }
    }
}
