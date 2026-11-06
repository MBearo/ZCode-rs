//! 代理驾驶舱（docs/specs/rust-cli/tui-cockpit.md）：← 的整页。它是 `/resume` 列表的同一个 `Overlay::Sessions`，所以改名、删除、
//! 读对话、⌃e、⌃b 都是 `sessions_steps.rs` 的；这里是它自己的：分组（`kind_of`，纯函数，每次重算）、次序、草稿里的筛选与键。
//! 行与窗格在 `cockpit_view.rs`，runner 的部分在 `runner/cockpit.rs`。
use std::collections::HashMap;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_kernel_api::message::{ContentBlock, Message, ToolResultContent};
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::panels::SessionActivity;
use rust_cli_kernel_api::peers::{PeerAddress, PeerAgent, PeerAnswer, PeerAsk, PeerPresence};
use rust_cli_kernel_api::session_api::SessionView;
use rust_cli_kernel_api::store::SessionOrder;

use super::app::{Action, App, Overlay};
use super::projects::Request;
use super::sessions_list::{Found, SessionsList};
use super::sessions_steps::Step;
use crate::services::CockpitNote;

/// 一个代理在哪一组（tui-cockpit.md §1.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Need,
    Work,
    New,
    Failed,
    Rest,
}

impl Kind {
    /// ⌃f 的次序：全部之后是这五种，再回到全部。
    fn next(now: Option<Self>) -> Option<Self> {
        match now {
            None => Some(Self::Need),
            Some(Self::Need) => Some(Self::Work),
            Some(Self::Work) => Some(Self::New),
            Some(Self::New) => Some(Self::Failed),
            Some(Self::Failed) => Some(Self::Rest),
            Some(Self::Rest) => None,
        }
    }

    /// 页上的组：做完的与失败的同一组。
    pub(super) fn group(self) -> usize {
        match self {
            Self::Need => 0,
            Self::Work => 1,
            Self::New | Self::Failed => 2,
            Self::Rest => 3,
        }
    }
}

/// 状态不另存：内核给的状态与笔记每次重算（§1.1）。比较 `endedAt > seenAt`，不比较 `updatedAt`：改名、写标题不算新东西。
pub fn kind_of(x: &Found) -> Kind {
    kind_from(x.activity, &x.note)
}

/// [`kind_of`] 的内核状态与笔记：槽的提示在 runner 里按同一个规则算（§3.4），不用先做出整行。
pub fn kind_from(activity: Option<SessionActivity>, note: &CockpitNote) -> Kind {
    match activity {
        Some(SessionActivity::NeedsInput) => Kind::Need,
        Some(SessionActivity::Working) => Kind::Work,
        _ => {
            let fresh = note
                .ended_at_ms
                .is_some_and(|ended| note.seen_at_ms.is_none_or(|seen| ended > seen));
            match (fresh, note.failed.is_some()) {
                (true, true) => Kind::Failed,
                (true, false) => Kind::New,
                _ => Kind::Rest,
            }
        }
    }
}

/// 一组的数（页的头一行，§1.1；槽的提示，§3.4）。
pub(super) fn count_words(lang: super::i18n::Lang, kind: Kind, n: usize) -> String {
    use super::i18n::Lang;
    match (kind, lang) {
        (Kind::Need, Lang::En) if n == 1 => "1 needs you".to_owned(),
        (Kind::Need, Lang::En) => format!("{n} need you"),
        (Kind::Work, Lang::En) => format!("{n} working"),
        (Kind::New, Lang::En) => format!("{n} new"),
        (Kind::Failed, Lang::En) => format!("{n} failed"),
        (Kind::Rest, Lang::En) => format!("{n} resting"),
        (Kind::Need, Lang::Zh) => format!("{n} 个等你"),
        (Kind::Work, Lang::Zh) => format!("{n} 个在跑"),
        (Kind::New, Lang::Zh) => format!("{n} 个新的"),
        (Kind::Failed, Lang::Zh) => format!("{n} 个失败"),
        (Kind::Rest, Lang::Zh) => format!("{n} 个歇着"),
    }
}

/// 槽的提示（§3.4）：别的代理按急的次序第一种不为 0 的组与它的数。Working 与 Resting 不算消息；不数现在的会话。
pub fn news_of(kinds: &[(String, Kind)], current: Option<&str>) -> Option<(Kind, usize)> {
    [Kind::Need, Kind::Failed, Kind::New]
        .into_iter()
        .find_map(|kind| {
            let n = kinds
                .iter()
                .filter(|(id, k)| *k == kind && Some(id.as_str()) != current)
                .count();
            (n > 0).then_some((kind, n))
        })
}

/// 行的年龄从哪一刻算（§1.1 的表）。
pub fn since_of(x: &Found) -> u64 {
    match kind_of(x) {
        Kind::Need | Kind::Work => x
            .presence
            .as_ref()
            .and_then(|p| p.since_ms)
            .unwrap_or(x.updated),
        Kind::New | Kind::Failed => x.note.ended_at_ms.unwrap_or(x.updated),
        Kind::Rest => x.updated,
    }
}

/// 词（已小写）命中这个代理的哪里（§2.3）。
fn admits(x: &Found, lower: &str) -> bool {
    lower.is_empty()
        || [
            x.title.as_str(),
            x.root.as_str(),
            x.folder.as_str(),
            x.branch.as_deref().unwrap_or_default(),
            x.prompt.as_str(),
            x.reply.as_str(),
        ]
        .iter()
        .any(|text| text.to_lowercase().contains(lower))
}

/// 选中的代理的窗格里，只有库里才有的那部分（`session_snapshot`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Peek {
    pub id: String,
    pub model: String,
    pub context: Option<u32>,
    pub reply: String,
    /// 最后几行：（是不是工具行，字）。
    pub tail: Vec<(bool, String)>,
}

/// 窗格里最后的这么多行（§2.2）。
const TAIL_LINES: usize = 4;

/// 读回的会话的窗格部分。
pub fn peek_of(id: String, view: &SessionView) -> Peek {
    let mut tail = Vec::new();
    let mut reply = String::new();
    for message in &view.messages {
        let content = match message {
            Message::Assistant(message) => &message.content,
            Message::User(message) => &message.content,
            _ => continue,
        };
        let assistant = matches!(message, Message::Assistant(_));
        for block in content {
            match block {
                ContentBlock::Text { text } if assistant && !text.trim().is_empty() => {
                    reply.clone_from(text);
                    let last = text.lines().rfind(|line| !line.trim().is_empty());
                    tail.push((false, last.unwrap_or_default().trim().to_owned()));
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    let summary = super::interaction_ask::summary_of(name, input);
                    let name = super::interaction_ask::display_name(name);
                    tail.push((true, format!("{name}  {summary}").trim_end().to_owned()));
                }
                ContentBlock::ToolResult { content, .. } => {
                    let text = content.iter().rev().find_map(|part| match part {
                        ToolResultContent::Text { text } => text
                            .lines()
                            .rfind(|line| !line.trim().is_empty())
                            .map(str::to_owned),
                        _ => None,
                    });
                    tail.extend(text.map(|text| (false, format!("  {}", text.trim()))));
                }
                _ => {}
            }
        }
    }
    let keep = tail.len().saturating_sub(TAIL_LINES);
    tail.drain(..keep);
    let selection = &view.selection;
    let model = match &selection.reasoning {
        Some(reasoning) => format!("{} {reasoning}", selection.model),
        None => selection.model.clone(),
    };
    let context = view
        .context_usage
        .filter(|usage| usage.max_tokens > 0)
        .map(|usage| (usage.used_tokens * 100 / usage.max_tokens) as u32);
    Peek {
        id,
        model,
        context,
        reply,
        tail,
    }
}

/// 页自己的状态（其余在 `SessionsList`：`query` 是草稿里的词，`folders` 总是真）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Cockpit {
    /// ⇥：按文件夹（记在 `puffin-tui/cockpit.json`，§2.1）。
    pub by_folder: bool,
    /// ⌃f 筛的组。
    pub kind: Option<Kind>,
    /// runner 读来的笔记（行收下时填进 `Found::note`）。
    pub notes: HashMap<String, CockpitNote>,
    pub peek: Option<Peek>,
    /// 最后一次要窗格时的（会话，更新的时刻，正在做的一行）：变了才再读。
    pub peek_for: Option<(String, u64, Option<String>)>,
    /// 最后的回复只在第一次有词时读一次（§2.3）。
    pub replies_asked: bool,
    /// 上一帧每个代理的组与它换组的时刻（换了组的行淡入，§2.1）。
    pub kinds: HashMap<String, (Kind, Instant)>,
    /// 窗格现在的代理与它换上来的时刻（§2.2）。
    pub shown: Option<(String, Instant)>,
}

impl SessionsList {
    /// 页上的行（§1.1、§2.1）：组内 Needs you 等得最久的在前，其余最近的在前（⌃e 时按创建时间）；按文件夹时组按里面最急的
    /// 代理排，组里保留这个次序。↑↓ 与窗口都按它。
    pub(super) fn cockpit_order<'a>(&'a self, cockpit: &Cockpit) -> Vec<&'a Found> {
        let Some(all) = &self.all else {
            return Vec::new();
        };
        let lower = self.query.to_lowercase();
        let mut shown: Vec<&Found> = all
            .iter()
            .filter(|x| admits(x, &lower))
            .filter(|x| cockpit.kind.is_none_or(|kind| kind == kind_of(x)))
            .collect();
        let created = self.order == SessionOrder::Created;
        shown.sort_by_key(|x| {
            let group = kind_of(x).group();
            let at = if created { x.created } else { since_of(x) };
            // 等得最久的在前，其余最近的在前。
            (group, if group == 0 { at } else { u64::MAX - at })
        });
        if !cockpit.by_folder {
            return shown;
        }
        let mut groups: Vec<Vec<&Found>> = Vec::new();
        for x in shown {
            match groups.iter_mut().find(|g| g[0].root == x.root) {
                Some(group) => group.push(x),
                None => groups.push(vec![x]),
            }
        }
        groups.into_iter().flatten().collect()
    }

    /// 选中的代理还在就留着它，否则第一行。
    fn cockpit_keep(&mut self, was: Option<String>) {
        self.choice = was
            .and_then(|id| self.matches().iter().position(|x| x.id == id))
            .unwrap_or(0);
    }
}

impl App {
    fn cockpit_list(&mut self) -> Option<&mut SessionsList> {
        match self.overlay.as_mut() {
            Some(Overlay::Sessions(list)) if list.cockpit.is_some() => Some(list),
            _ => None,
        }
    }

    pub fn cockpit_open(&self) -> bool {
        matches!(&self.overlay, Some(Overlay::Sessions(list)) if list.cockpit.is_some())
    }

    /// 输入框为空时的 ← 与 `/cockpit`：第一次打开选第一行（最急的）；数据到之前只有头一行。
    pub(super) fn open_cockpit(&mut self) -> Option<Action> {
        self.composer.clear();
        self.overlay = Some(Overlay::Sessions(Box::new(SessionsList {
            folders: true,
            cockpit: Some(Box::new(Cockpit {
                by_folder: self.cockpit_by_folder,
                ..Cockpit::default()
            })),
            ..SessionsList::default()
        })));
        Some(Action::Projects(Request::LoadResume {
            root: None,
            order: SessionOrder::default(),
        }))
    }

    /// 回去：页关上，回到原来的 transcript 与滚动位置，现在的会话算看过（§1.2）。
    pub(super) fn cockpit_leave(&mut self) -> Option<Action> {
        self.overlay = None;
        self.composer.clear();
        Some(Action::Projects(Request::Seen(self.session.clone()?)))
    }

    /// runner 读来的笔记：之后收下的行带着它（`with_folders`）。
    pub fn cockpit_notes(&mut self, notes: HashMap<String, CockpitNote>) {
        if let Some(Overlay::Sessions(list)) = self.overlay.as_mut()
            && let Some(cockpit) = list.cockpit.as_mut()
        {
            cockpit.notes = notes;
        }
    }

    /// 选中的代理的窗格要不要再读（选中的换了，或它的时刻、正在做的一行变了）：runner 在键与收下之后问。
    pub fn cockpit_peek_wanted(&mut self) -> Option<String> {
        let list = self.cockpit_list()?;
        let x = list.chosen_found()?;
        let key = (
            x.id.clone(),
            x.updated,
            x.presence.as_ref().and_then(|p| p.presence.now.clone()),
        );
        let cockpit = list.cockpit.as_mut()?;
        if cockpit.peek_for.as_ref() == Some(&key) {
            return None;
        }
        let id = key.0.clone();
        cockpit.peek_for = Some(key);
        Some(id)
    }

    pub fn cockpit_peeked(&mut self, peek: Peek) {
        if let Some(cockpit) = self.cockpit_list().and_then(|l| l.cockpit.as_mut()) {
            cockpit.peek = Some(peek);
        }
    }

    /// 最后的回复读到了（§2.3）：填进行，再筛一次，选中的留着。
    pub fn cockpit_replies(&mut self, replies: Vec<(String, String)>) {
        let Some(list) = self.cockpit_list() else {
            return;
        };
        let was = list.chosen_found().map(|x| x.id.clone());
        if let Some(all) = list.all.as_mut() {
            for (id, reply) in replies {
                if let Some(x) = all.iter_mut().find(|x| x.id == id) {
                    x.reply = reply;
                }
            }
        }
        list.cockpit_keep(was);
    }

    /// 草稿变了（`settle_sessions`）：词筛行。打字不进任何面板（§2.3）；粘贴也不进 shell 模式。
    pub(super) fn cockpit_settle(&mut self, text: &str) -> Option<Action> {
        self.composer.popup = None;
        self.composer.mode = super::composer::InputMode::Prompt;
        let query = text.trim().to_owned();
        let list = self.cockpit_list()?;
        if list.query == query {
            return None;
        }
        let was = list.chosen_found().map(|x| x.id.clone());
        list.query = query.clone();
        list.cockpit_keep(was);
        let missing: Vec<String> = list
            .all
            .iter()
            .flatten()
            .filter(|x| x.reply.is_empty())
            .map(|x| x.id.clone())
            .collect();
        let cockpit = list.cockpit.as_mut()?;
        if query.is_empty() || std::mem::replace(&mut cockpit.replies_asked, true) {
            return None;
        }
        Some(Action::Projects(Request::Replies(missing)))
    }

    /// 页拿的键（`sessions_key` 先问它，只在列表这一步）。`Some` 到此为止，`None` 让 `sessions_key` 接着问，它不拿的去编辑
    /// 草稿。现在的会话的请求开着时键归请求（§3.2），页只留着 ←。
    pub(super) fn cockpit_key(&mut self, key: &KeyEvent) -> Option<Option<Action>> {
        let empty = self.composer.area.is_empty();
        if self.asking() {
            return (key.code == KeyCode::Left && empty).then(|| self.cockpit_leave());
        }
        let ctrl = crate::ui::keyboard::ctrl(key);
        let lang = self.lang;
        let plain = crate::ui::keyboard::is_plain_text(key);
        let list = self.cockpit_list()?;
        match key.code {
            KeyCode::Esc if !empty => self.composer.clear(),
            KeyCode::Esc => return Some(self.cockpit_leave()),
            KeyCode::Left if empty => return Some(self.cockpit_leave()),
            KeyCode::Right if empty => return None,
            KeyCode::Tab => {
                let was = list.chosen_found().map(|x| x.id.clone());
                let cockpit = list.cockpit.as_mut()?;
                cockpit.by_folder = !cockpit.by_folder;
                let by_folder = cockpit.by_folder;
                list.cockpit_keep(was);
                self.cockpit_by_folder = by_folder;
                return Some(Some(Action::Projects(Request::RememberGrouping(by_folder))));
            }
            KeyCode::Char('f') if ctrl => {
                let was = list.chosen_found().map(|x| x.id.clone());
                let cockpit = list.cockpit.as_mut()?;
                cockpit.kind = Kind::next(cockpit.kind);
                list.cockpit_keep(was);
            }
            KeyCode::Char('x') if ctrl => {
                let x = list.chosen_found()?.clone();
                if !matches!(kind_of(&x), Kind::Need | Kind::Work) {
                    self.said.quiet =
                        Some(say(lang, "Nothing to stop.", "没有在跑的。").to_owned());
                    return Some(None);
                }
                let address: Option<PeerAddress> = x
                    .presence
                    .filter(|presence| !presence.here)
                    .map(|presence| presence.address);
                return Some(Some(Action::Projects(Request::Stop { address })));
            }
            // 打字只进草稿：`/ ! @ $` 都是筛选的字，`?` 在词为空时是按键表（§2.3）。
            KeyCode::Char('?') if plain && empty => list.step = Step::Keys,
            KeyCode::Char(c) if plain => self.composer.area.insert(c),
            _ => return None,
        }
        Some(None)
    }
}

/// 现在的会话的现场（§4）：runner 变了才写。有上限：问题与 detail 各 500 字符，回答至多 9 个，子代理至多 12 个，`now` 200 字符。
pub fn presence_of(app: &App, now: Option<&(String, Option<String>)>) -> PeerPresence {
    let clip = |text: &str, most: usize| text.chars().take(most).collect::<String>();
    let view = &app.view;
    let tasks = view.tasks();
    let done = tasks
        .iter()
        .filter(|task| task.status == rust_cli_kernel_api::agents::TaskStatus::Completed)
        .count();
    PeerPresence {
        now: now.map(|(line, _)| clip(line, 200)),
        machine: now.and_then(|(_, machine)| machine.clone()),
        ask: view.pending().map(|(_, request)| ask_of(request, app.lang)),
        agents: app
            .snapshots
            .agents
            .entries
            .iter()
            .take(12)
            .map(|entry| PeerAgent {
                name: entry.agent_type.clone(),
                doing: clip(&entry.description, 200),
                running: entry.status == rust_cli_kernel_api::panels::SubagentStatus::Running,
                elapsed_ms: entry.elapsed_ms,
                at_ms: 0,
            })
            .collect(),
        todos: (!tasks.is_empty()).then_some((done as u32, tasks.len() as u32)),
        context: view
            .context_usage()
            .filter(|usage| usage.max_tokens > 0)
            .map(|usage| (usage.used_tokens * 100 / usage.max_tokens) as u32),
    }
}

/// 请求的问题与回答（只给人看，§2.2）。
fn ask_of(request: &InteractionRequest, lang: super::i18n::Lang) -> PeerAsk {
    let clip = |text: &str| text.chars().take(500).collect::<String>();
    let answer = |label: &str, note: &str| PeerAnswer {
        label: label.to_owned(),
        note: note.to_owned(),
    };
    match request {
        InteractionRequest::Permission {
            tool,
            input,
            message,
            ..
        } => {
            let bash = tool == "Bash";
            let question = if bash {
                say(lang, "Run this command?", "运行这个命令？").to_owned()
            } else {
                message.clone()
            };
            PeerAsk {
                question: clip(&question),
                detail: bash.then(|| clip(&super::interaction_ask::summary_of(tool, input))),
                answers: vec![
                    answer(say(lang, "Allow once", "允许一次"), ""),
                    answer(say(lang, "Always allow", "总是允许"), ""),
                    answer(say(lang, "Deny", "拒绝"), ""),
                ],
            }
        }
        InteractionRequest::Question { questions, .. } => {
            let first = &questions[0];
            PeerAsk {
                question: clip(first["question"].as_str().unwrap_or_default()),
                detail: None,
                answers: first["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(9)
                    .map(|option| {
                        answer(
                            option["label"].as_str().unwrap_or_default(),
                            option["description"].as_str().unwrap_or_default(),
                        )
                    })
                    .collect(),
            }
        }
        InteractionRequest::PlanApproval { .. } => PeerAsk {
            question: say(lang, "Approve this plan?", "批准这个计划？").to_owned(),
            detail: None,
            answers: vec![
                answer(say(lang, "Approve", "批准"), ""),
                answer(say(lang, "Keep planning", "继续计划"), ""),
            ],
        },
    }
}

pub(super) fn say(lang: super::i18n::Lang, en: &'static str, zh: &'static str) -> &'static str {
    if lang == super::i18n::Lang::Zh {
        zh
    } else {
        en
    }
}

#[cfg(test)]
#[path = "cockpit_tests.rs"]
mod tests;
