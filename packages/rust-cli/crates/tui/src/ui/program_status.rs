//! 程序状态（OSC 7501，Program Status Protocol 0.4；docs/specs/rust-cli/tui-manage.md §5.4）：把“现在在做什么”报告给
//! 终端。终端按 id 各存一条记录，自己决定怎么显示。
//!
//! - 想要的记录每次画帧前由界面状态算出（[`wanted`]：询问、回合行、后台任务快照），和上次写出的不同才写（[`ProgramStatus::bytes`]），
//!   与窗口标题同一种做法；不另存业务状态、不用计时器。
//! - 唯一另记的是“这次接上会话之后看到过 `TurnFinished`”：打开旧会话时它上次的结局不算“还没看的结果”。
//! - 终端支不支持由进界面时的探测定（`probe.rs`，[`supported`]）；不支持时 runner 一个序列都不写。
use std::collections::{BTreeMap, BTreeSet};

use base64::Engine;
use rust_cli_base::chars;
use rust_cli_kernel_api::env_shell::BackgroundStatus;
use rust_cli_kernel_api::manage::BackgroundKind;
use rust_cli_kernel_api::orchestration::InteractionRequest;

use super::app::App;
use super::notify::ask_text;
use crate::view::TurnState;

/// 探测的查询：问终端支不支持（协议的 feature detection）。
pub const QUERY: &str = "\x1b]7501;?\x1b\\";
/// 支持的终端的回复以它开头（之后可能有 `:states=…:kinds=…`）。
const REPLY: &str = "\x1b]7501;?";
/// 报告的开头与结尾（ST）。
const OPEN: &str = "\x1b]7501;";
const ST: &str = "\x1b\\";
/// 根记录的 `app`：程序的稳定标识，终端按它分组、选图标。
const APP: &str = "zcode";
/// 协议的上限（解码后的字节）：`msg` 2048、`title` 192。截到上限之内，终端不会整条丢弃。
const MSG_MAX: usize = 2048;
const TITLE_MAX: usize = 192;
/// 后台子代理的子记录 id：前缀加 agentId 的 FNV-1a（16 位十六进制，满足 id 的字符集与每段 32 字节的上限）。
const AGENT_ID_PREFIX: &str = "agent-";

/// 探测的回复里有 7501 的回答：终端支持。
pub fn supported(reply: &str) -> bool {
    reply.contains(REPLY)
}

/// 被挡住时在等什么（协议的 `kind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 权限与计划审批：批准做某件事。
    Permission,
    /// 提问：用户要给出回答。
    Question,
}

/// 记录的状态（协议的 `state`，不含 `clear`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Working,
    Done,
    Blocked(Kind),
    Error,
}

impl State {
    fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Blocked(_) => "blocked",
            Self::Error => "error",
        }
    }
}

/// 一条想要的记录。`id` 为 None 是根记录；`title`、`msg` 是原文（编码时清理、截断，空的不写）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub id: Option<String>,
    pub state: State,
    pub title: String,
    pub msg: String,
}

impl Report {
    fn root(state: State, msg: impl Into<String>) -> Self {
        Self {
            id: None,
            state,
            title: String::new(),
            msg: msg.into(),
        }
    }

    /// 报告的正文：`键=值` 以 `:` 分隔，键的顺序固定。根记录每次都带 `app`（每次报告整条替换记录）。
    fn body(&self) -> String {
        let mut pairs = vec![format!("state={}", self.state.name())];
        if let State::Blocked(kind) = self.state {
            let kind = match kind {
                Kind::Permission => "permission",
                Kind::Question => "question",
            };
            pairs.push(format!("kind={kind}"));
        }
        match &self.id {
            Some(id) => pairs.push(format!("id={id}")),
            None => pairs.push(format!("app={APP}")),
        }
        for (key, text, max) in [
            ("title", &self.title, TITLE_MAX),
            ("msg", &self.msg, MSG_MAX),
        ] {
            let text = clean(text, max);
            if !text.is_empty() {
                let encoded = base64::engine::general_purpose::STANDARD.encode(text);
                pairs.push(format!("{key}={encoded}"));
            }
        }
        pairs.join(":")
    }
}

/// 一段报告的字节。
fn sequence(body: &str) -> String {
    format!("{OPEN}{body}{ST}")
}

/// 协议的文字只许一行、不许控制字符：控制字符与行、段分隔符换成空格，双向与零宽字符去掉，去掉首尾空白，按字符边界截到
/// `max` 字节之内。
fn clean(text: &str, max: usize) -> String {
    let single: String = text
        .chars()
        .filter(|c| !chars::is_bidi_or_zero_width(*c))
        .map(|c| {
            if chars::is_c0(c) || chars::is_del_or_c1(c) || chars::is_line_break_separator(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    let trimmed = single.trim();
    let mut end = trimmed.len().min(max);
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    trimmed[..end].trim_end().to_owned()
}

/// 后台子代理的子记录 id。
fn agent_id(agent: &str) -> String {
    format!(
        "{AGENT_ID_PREFIX}{:016x}",
        rust_cli_base::hash::fnv1a_64(agent.as_bytes())
    )
}

/// 询问在等什么与它的说法（§5.1 的说法；权限再加工具名）。
fn blocked(request: &InteractionRequest, app: &App) -> Report {
    let text = ask_text(request, app.lang);
    match request {
        InteractionRequest::Permission { tool, .. } => {
            Report::root(State::Blocked(Kind::Permission), format!("{text}: {tool}"))
        }
        InteractionRequest::PlanApproval { .. } => {
            Report::root(State::Blocked(Kind::Permission), text)
        }
        InteractionRequest::Question { .. } => Report::root(State::Blocked(Kind::Question), text),
    }
}

/// 根记录（按顺序取第一个）：开着的询问、在跑的回合、亲眼看到结束的回合的结局、其余是 idle。
fn root(app: &App) -> Report {
    if let Some((_, request)) = app.pending() {
        return blocked(request, app);
    }
    let title = app.view.title();
    let seen = app.program_status.finished;
    match app.view.turn_state() {
        Some(TurnState::Running) => Report::root(State::Working, title),
        Some(TurnState::Success) if seen => Report::root(State::Done, title),
        Some(TurnState::Failed) if seen => Report::root(
            State::Error,
            app.view
                .last_failure()
                .and_then(|message| message.lines().next())
                .unwrap_or_default(),
        ),
        _ => Report::root(State::Idle, ""),
    }
}

/// 这一刻想要的全部记录：根记录，加上每个在跑的后台子代理一条子记录。后台命令与 Monitor 不报（一直“在跑”的服务与监视器
/// 不是在做事）。
pub fn wanted(app: &App) -> Vec<Report> {
    let agents = app.snapshots.background.iter().filter(|entry| {
        entry.kind == BackgroundKind::Agent && entry.status == BackgroundStatus::Running
    });
    std::iter::once(root(app))
        .chain(agents.map(|entry| Report {
            id: Some(agent_id(&entry.id)),
            state: State::Working,
            title: entry.description.clone(),
            msg: String::new(),
        }))
        .collect()
}

/// 程序状态的界面状态：看到过 `TurnFinished` 没有，与上次写到终端的每条记录（id → 正文）。
#[derive(Debug, Clone, Default)]
pub struct ProgramStatus {
    finished: bool,
    sent: BTreeMap<Option<String>, String>,
}

impl ProgramStatus {
    /// 接上会话（新建、切换、`--resume`）：这个会话的结局还没亲眼看到过。
    pub fn attached(&mut self) {
        self.finished = false;
    }

    /// 收到当前会话的 `TurnFinished`。
    pub fn turn_finished(&mut self) {
        self.finished = true;
    }

    /// 要写到终端的字节：不再想要的子记录写 `clear`，变了的记录写新的报告；什么都没变时为 None。
    pub fn bytes(&mut self, wanted: &[Report]) -> Option<String> {
        let ids: BTreeSet<&Option<String>> = wanted.iter().map(|report| &report.id).collect();
        let gone: Vec<Option<String>> = self
            .sent
            .keys()
            .filter(|id| !ids.contains(id))
            .cloned()
            .collect();
        let mut out = String::new();
        for id in gone {
            self.sent.remove(&id);
            out.push_str(&sequence(&match &id {
                Some(id) => format!("state=clear:id={id}"),
                None => "state=clear".to_owned(),
            }));
        }
        for report in wanted {
            let body = report.body();
            if self.sent.get(&report.id) != Some(&body) {
                out.push_str(&sequence(&body));
                self.sent.insert(report.id.clone(), body);
            }
        }
        (!out.is_empty()).then_some(out)
    }

    /// 离开界面：写过记录就清掉这个终端上的全部记录（不带 id 的 `clear`）。
    pub fn leave(&mut self) -> Option<String> {
        if self.sent.is_empty() {
            return None;
        }
        self.sent.clear();
        Some(sequence("state=clear"))
    }
}

#[cfg(test)]
#[path = "program_status_tests.rs"]
mod tests;
