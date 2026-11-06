//! 请求开着时的按键与它到达、离开时的界面状态（docs/puffin-tui.md §4.2「请求」「提问」「计划」「按键」；壳的
//! `shell/asks.rs` 的 `asked`、`settle_ask`、`ask_key`、`answer_key`、`question_key`、`give`、`advance_page`）。
//!
//! 请求拿 ↑↓ ^P ^N ⏎ 1–9 esc（提问另拿空输入框时的 ←→ 与空格）；别的键照常编辑输入框、滚动，⇧⇥ 照常换模式。esc 停下
//! 回合（agent 收回全部开着的请求）。回答发出之后选中的行一直亮着，回答的键不起作用，直到它离开视图。
//!
//! 与 Puffin 的不同（旧界面多出的能力，tui-kit.md §4.3）：
//! - Bash 的“不再询问”可以改前缀（工作线 F72）：选中 Always allow 时 ⇥ 把前缀放进输入框，⏎ 记住改过的前缀。
//! - 旧会话的询问（agents.md §4.1.2）排在当前会话的之后，一样回答。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::ids::{InteractionId, MachineName};
use rust_cli_kernel_api::orchestration::{self, InteractionRequest};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_tui_kit::app::widgets::asks::data::{Ask, AskOptionKind};
use rust_cli_tui_kit::app::widgets::asks::{self, Answer, AnswerKind, MAX_WORDS};

use super::Interaction;
use crate::ui::app::{Action, App};
use crate::ui::i18n_asks as words;
use crate::ui::interaction_ask as shape;
use crate::ui::interaction_view::open_asks;

/// 字符串的前 `n` 个码点。
fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// 键的结果：`None` 把键传下去（打字、滚动）；`Some(None)` 拿了、没有动作。
type Taken = Option<Option<Action>>;

impl App {
    /// 视图变化之后（Puffin `asked`）：刚到的请求记下时刻（它的 `●` 从这一刻淡入，终端在后台时响铃），在回答的跟着最早的
    /// 那个，没经我们回答就离开的说出来。
    pub fn sync_asks(&mut self, now: Instant) {
        let open: Vec<InteractionId> = open_asks(self)
            .into_iter()
            .map(|(id, _)| id.clone())
            .collect();
        let head = open.first().cloned();
        // 在回答的请求离开了：不是我们答的、回合还在进行、也不是 esc 停下的，那就是别处答的。
        let left = self
            .interaction
            .as_ref()
            .filter(|ui| head.as_ref() != Some(&ui.id))
            .map(|ui| ui.id.clone());
        if let Some(id) = left
            && !open.contains(&id)
            && !self.asks.answered.contains(&id)
            && self.running()
            && !self.asks.stopping
        {
            self.said.quiet = Some(words::answered_elsewhere(self.lang).to_owned());
        }
        let fresh: Vec<InteractionId> = open
            .iter()
            .filter(|id| !self.asks.seen.contains_key(*id))
            .cloned()
            .collect();
        self.asks.seen.retain(|id, _| open.contains(id));
        for id in &fresh {
            self.asks.seen.insert(id.clone(), now);
        }
        self.asks.answered.retain(|id| open.contains(id));
        if head.is_none() {
            self.asks.stopping = false;
        }
        self.asks.fresh.extend(fresh);
        self.open_head(now);
        self.settle_yolo();
        self.sync_peer(now);
    }

    /// 在回答的请求换了：新的界面状态（工作目录、远端机器、旧会话的标题）；没有请求时关掉。
    fn open_head(&mut self, now: Instant) {
        let Some((id, request)) = self.pending() else {
            self.interaction = None;
            return;
        };
        if self.interaction.as_ref().is_some_and(|ui| ui.id == *id) {
            return;
        }
        let (id, request) = (id.clone(), request.clone());
        let mut ui = Interaction::new(id.clone(), &request, now);
        ui.mode = self.view.mode();
        ui.workspace = self.workspace().to_owned();
        // 远端执行 F98（remote-exec.md §25.4.3）：远端的调用，“不再询问”写那台机器的工作目录；MCP 工具的机器来自服务器
        // 的绑定。
        if let InteractionRequest::Permission {
            tool,
            input,
            machine,
            ..
        } = &request
        {
            if machine.is_primary() {
                ui.bound_machine = crate::ui::machines::remote_of(tool, input, &self.mcp_machines)
                    .map(MachineName::new)
                    .unwrap_or_default();
            }
            if let Some(remote) = machine.remote().or(ui.bound_machine.remote()) {
                ui.workspace = self
                    .remote_root(remote)
                    .unwrap_or_else(|| remote.to_owned());
            }
        }
        ui.origin = self.detached_title(&id).map(str::to_owned);
        self.interaction = Some(ui);
        // 输入框已经有字时从 Deny / Keep planning 开始（§4.2「草稿」）。
        if let Some(ask) = self.head_kit_ask()
            && !self.composer.area.text().trim().is_empty()
            && !asks::is_question(&ask)
        {
            let choice = asks::typed_choice(&ask, &self.answers_of(&ask));
            if let Some(ui) = self.interaction.as_mut() {
                ui.choice = choice;
            }
        }
    }

    /// 一个键之后：打进空输入框的字选中带着它们的回答（Deny、Keep planning）；前缀删光了就不再是前缀（Puffin
    /// `settle_ask`）。`before` 是按键之前输入框的字。
    pub fn settle_ask(&mut self, before: &str) {
        if let Some(ui) = self.interaction.as_mut()
            && ui.editing
            && self.composer.area.text().trim().is_empty()
        {
            ui.editing = false;
        }
        let Some(ask) = self.head_kit_ask().filter(|a| !asks::is_question(a)) else {
            return;
        };
        if self.interaction.as_ref().is_none_or(|ui| ui.editing)
            || self.composer.area.text().trim().is_empty()
            || !before.trim().is_empty()
        {
            return;
        }
        let choice = asks::typed_choice(&ask, &self.answers_of(&ask));
        if let Some(ui) = self.interaction.as_mut() {
            ui.choice = choice;
        }
    }

    /// 请求拿的键；`None` 把键传下去。
    pub(in crate::ui) fn ask_key(&mut self, key: KeyEvent, now: Instant) -> Taken {
        let ask = self.head_kit_ask()?;
        let id = self.interaction.as_ref()?.id.clone();
        let ctrl = crate::ui::keyboard::ctrl(&key);
        let empty = self.composer.area.text().is_empty();
        let digit = |c: char| ('1'..='9').contains(&c);
        // ⇧⇥ 照常换模式，请求留着；^C 照常（中断、清空、退出）。
        if key.code == KeyCode::BackTab || (ctrl && key.code == KeyCode::Char('c')) {
            return None;
        }
        if key.code == KeyCode::Esc {
            self.asks.stopping = true;
            return Some(Some(self.interrupt()));
        }
        let up = key.code == KeyCode::Up || (ctrl && key.code == KeyCode::Char('p'));
        let down = key.code == KeyCode::Down || (ctrl && key.code == KeyCode::Char('n'));
        if self.asks.answered.contains(&id) {
            // 已经回答：等它离开视图，回答的按键不起作用。
            return match key.code {
                KeyCode::Enter | KeyCode::Tab | KeyCode::Left | KeyCode::Right => Some(None),
                _ if up || down => Some(None),
                KeyCode::Char(c) if empty && !ctrl && (digit(c) || c == ' ') => Some(None),
                _ => self.ask_plain(key, empty),
            };
        }
        let taken = if asks::is_question(&ask) {
            self.question_key(key, &ask, up, down, now)
        } else {
            self.answer_key(key, &ask, up, down, now)
        };
        taken.or_else(|| self.ask_plain(key, empty))
    }

    /// 请求开着时传下去之前要拦的键：`!` 是普通的字（请求开着时没有 shell 模式），← 与 ↓ 不打开项目页、后台与子代理的
    /// 列表（请求开着时别的层不开，Puffin `asking` 的门）。
    fn ask_plain(&mut self, key: KeyEvent, empty: bool) -> Taken {
        match key.code {
            KeyCode::Char('!')
                if empty && key.modifiers.difference(KeyModifiers::SHIFT).is_empty() =>
            {
                self.composer.area.insert('!');
                Some(None)
            }
            KeyCode::Left | KeyCode::Down if empty => Some(None),
            _ => None,
        }
    }

    /// 权限请求或计划：选、回答，或打字拒绝（§4.2「按键」）。
    fn answer_key(
        &mut self,
        key: KeyEvent,
        ask: &Ask,
        up: bool,
        down: bool,
        now: Instant,
    ) -> Taken {
        let answers = self.answers_of(ask);
        let n = answers.len().max(1);
        let armed = self.interaction.as_ref()?.armed(now);
        let typed = self.composer.area.text().trim().to_owned();
        if up || down {
            let ui = self.interaction.as_mut()?;
            ui.choice = if up {
                (ui.choice + n - 1) % n
            } else {
                (ui.choice + 1) % n
            };
            return Some(None);
        }
        match key.code {
            KeyCode::Char(c)
                if ('1'..='9').contains(&c) && self.composer.area.text().is_empty() =>
            {
                let i = (c as usize) - ('1' as usize);
                let ok = answers
                    .get(i)
                    .is_some_and(|a| a.kind != AnswerKind::Keep && armed);
                Some(if ok {
                    self.give(ask, &answers, i, "")
                } else {
                    None
                })
            }
            // 旧界面多出的：选中 Always allow 时 ⇥ 把“不再询问”的前缀放进输入框（自拟，tui-kit.md §4.3）。
            KeyCode::Tab if self.composer.area.text().is_empty() => {
                let choice = self.interaction.as_ref()?.choice;
                let always = answers
                    .get(choice)
                    .is_some_and(|a| a.kind == AnswerKind::Option(AskOptionKind::AllowAlways));
                let seed = self.ask_seed().filter(|_| always)?;
                self.composer.set_text(&seed);
                let ui = self.interaction.as_mut()?;
                ui.editing = true;
                ui.seed = Some(seed);
                Some(None)
            }
            KeyCode::Enter => {
                let choice = self.interaction.as_ref()?.choice;
                let editing = self.interaction.as_ref()?.editing;
                Some(if !armed {
                    None
                } else if editing && !typed.is_empty() {
                    // 改过的前缀：记住它。
                    if let Some(ui) = self.interaction.as_mut() {
                        ui.seed = Some(typed.clone());
                    }
                    let fresh = self.answers_of(ask);
                    let at = fresh
                        .iter()
                        .position(|a| a.kind == AnswerKind::Option(AskOptionKind::AllowAlways));
                    let given = at.and_then(|i| self.give(ask, &fresh, i, ""));
                    self.composer.clear();
                    if let Some(ui) = self.interaction.as_mut() {
                        ui.editing = false;
                    }
                    given
                } else if !typed.is_empty() {
                    let i = asks::typed_choice(ask, &answers);
                    self.give(ask, &answers, i, &typed)
                } else if answers
                    .get(choice)
                    .is_none_or(|a| a.kind == AnswerKind::Keep)
                {
                    None
                } else {
                    self.give(ask, &answers, choice, "")
                })
            }
            _ => None,
        }
    }

    /// 发出回答 `i`（Deny、Keep planning 带着 `typed`）；选中的行一直亮，直到请求离开视图。Full access 先经确认。
    fn give(&mut self, ask: &Ask, answers: &[Answer], i: usize, typed: &str) -> Option<Action> {
        let a = answers.get(i)?;
        let (id, request, seed, mode) = {
            let (id, request, ui) = self.head_ask()?;
            (id.clone(), request.clone(), ui.seed.clone(), ui.mode)
        };
        if let Some(ui) = self.interaction.as_mut() {
            ui.choice = i;
        }
        if a.kind == AnswerKind::Option(AskOptionKind::FullAccess) {
            self.confirm_yolo_for(id, a.id.clone());
            return None;
        }
        let text = take_chars(typed, MAX_WORDS);
        let sends: Vec<(InteractionId, orchestration::Answer)> = if a.kind == AnswerKind::AllowAll {
            self.allow_all_asks()
                .into_iter()
                .filter_map(|x| {
                    let request = open_asks(self)
                        .into_iter()
                        .find(|(o, _)| **o == x)
                        .map(|(_, r)| r.clone())?;
                    let answer = shape::option_answer(&request, shape::ALLOW_ONCE, None, None)?;
                    Some((x, answer))
                })
                .collect()
        } else if asks::is_plan(ask) {
            // 批准计划随之关掉计划模式时不说 `Plan mode is off.`：回答请求的地方记下（Puffin asks.rs 的 `plan_approved`）。
            self.status_motion.plan_approved |= a.kind == AnswerKind::Approve;
            let feedback = if a.kind == AnswerKind::Keep {
                text.as_str()
            } else {
                ""
            };
            vec![(id, shape::plan_answer(mode, feedback))]
        } else {
            let free = (a.kind == AnswerKind::Option(AskOptionKind::Deny) && !text.is_empty())
                .then_some(text.as_str());
            vec![(
                id,
                shape::option_answer(&request, &a.id, free, seed.as_deref())?,
            )]
        };
        // 带字的回答发出后输入框清空；其他回答不动草稿。
        if !text.is_empty() {
            self.composer.clear();
        }
        self.said.failed = None;
        Some(self.send_answers(sends))
    }

    /// 把回答交给 runner，记下它们已回答。
    fn send_answers(&mut self, sends: Vec<(InteractionId, orchestration::Answer)>) -> Action {
        let mut actions: Vec<Action> = sends
            .into_iter()
            .map(|(interaction, answer)| {
                self.asks.answered.insert(interaction.clone());
                Action::Resolve {
                    interaction,
                    answer,
                }
            })
            .collect();
        if actions.len() == 1 {
            actions.remove(0)
        } else {
            Action::Many(actions)
        }
    }

    /// 完全访问的确认之后（tui.md §7.8）：允许这一次，再切到完全访问。请求已经回答或离开时只切模式。
    pub(in crate::ui) fn answer_full_access(
        &mut self,
        ask: &InteractionId,
        option: &str,
    ) -> Action {
        let set = Action::SetMode(PermissionMode::BypassPermissions);
        if self.asks.answered.contains(ask) {
            return set;
        }
        let request = open_asks(self)
            .into_iter()
            .find(|(id, _)| *id == ask)
            .map(|(_, r)| r.clone());
        let Some(answer) = request.and_then(|r| shape::option_answer(&r, option, None, None))
        else {
            return set;
        };
        self.said.failed = None;
        let resolve = self.send_answers(vec![(ask.clone(), answer)]);
        Action::Many(vec![resolve, set])
    }

    /// 提问：一页上的回答、换页、你自己的话（§4.2「提问」）。
    fn question_key(
        &mut self,
        key: KeyEvent,
        ask: &Ask,
        up: bool,
        down: bool,
        now: Instant,
    ) -> Taken {
        let pages = asks::pages_of(ask);
        let armed = self.interaction.as_ref()?.armed(now);
        let empty = self.composer.area.text().is_empty();
        let typed = self.composer.area.text().trim().to_owned();
        let mut ui = self.interaction.take()?;
        let page = pages.get(ui.page);
        let n = page.map_or(0, |p| p.options.len());
        let multi = page.is_some_and(|p| p.multi);
        let mut advance = false;
        let taken = match key.code {
            _ if up && n > 0 => {
                ui.choice = (ui.choice + n - 1) % n;
                true
            }
            _ if down && n > 0 => {
                ui.choice = (ui.choice + 1) % n;
                true
            }
            KeyCode::Left | KeyCode::Right if empty => {
                let last = pages.len().saturating_sub(1);
                ui.page = if key.code == KeyCode::Right {
                    (ui.page + 1).min(last)
                } else {
                    ui.page.saturating_sub(1)
                };
                ui.choice = 0;
                true
            }
            KeyCode::Char(' ') if multi && empty && n > 0 => {
                let picked = ui.picks.entry(ui.page).or_default();
                if let Some(at) = picked.iter().position(|k| *k == ui.choice) {
                    picked.remove(at);
                } else {
                    picked.push(ui.choice);
                    picked.sort_unstable();
                }
                true
            }
            KeyCode::Char(c) if ('1'..='9').contains(&c) && empty && !multi => {
                let i = (c as usize) - ('1' as usize);
                if i < n && armed {
                    ui.picks.insert(ui.page, vec![i]);
                    ui.choice = i;
                    advance = true;
                }
                true
            }
            KeyCode::Enter => {
                if armed {
                    if !typed.is_empty() {
                        ui.own.insert(ui.page, take_chars(&typed, MAX_WORDS));
                        self.composer.clear();
                    } else if !multi && n > 0 {
                        ui.picks.insert(ui.page, vec![ui.choice]);
                    }
                    advance = true;
                }
                true
            }
            _ => false,
        };
        self.interaction = Some(ui);
        if !taken {
            return None;
        }
        Some(if advance {
            self.advance_page(ask)
        } else {
            None
        })
    }

    /// 下一页；最后一页之后交出全部回答。
    fn advance_page(&mut self, ask: &Ask) -> Option<Action> {
        let count = asks::pages_of(ask).len();
        let ui = self.interaction.as_mut()?;
        if ui.page + 1 < count {
            ui.page += 1;
            ui.choice = 0;
            return None;
        }
        let answer = self.question_value(ask, None)?;
        let id = ui_id(self)?;
        Some(self.send_answers(vec![(id, answer)]))
    }

    /// 提问的回答（逐页选中的回答与你自己的话）；空闲到期时带超时的毫秒数（tools-more.md §1.1）。带预览的一页上你自己的
    /// 话在选了回答时是备注（工作线 F72 的 `annotations.notes`，旧界面多出的），没选时是这一页的回答。
    pub(in crate::ui) fn question_value(
        &self,
        ask: &Ask,
        afk_timeout_ms: Option<u64>,
    ) -> Option<orchestration::Answer> {
        let ui = self.interaction.as_ref()?;
        let pages = asks::pages_of(ask);
        let mut notes = Vec::new();
        let values: Vec<Vec<String>> = pages
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let mut values: Vec<String> = ui
                    .picks
                    .get(&i)
                    .into_iter()
                    .flatten()
                    .filter_map(|k| p.options.get(*k).map(|o| o.value.clone()))
                    .collect();
                let own = ui.own.get(&i).filter(|s| !s.is_empty());
                let previews = !p.multi && p.options.iter().any(|o| !o.preview.is_empty());
                match own {
                    Some(own) if previews && !values.is_empty() => {
                        notes.push((p.question.clone(), own.clone()));
                    }
                    Some(own) => values.push(own.clone()),
                    None => {}
                }
                values
            })
            .collect();
        Some(shape::question_answer(
            pages,
            &values,
            notes,
            afk_timeout_ms,
        ))
    }

    /// 提问空闲到期：按当时已选的回答提交（tools-more.md §1.1）。
    pub fn idle_answer(
        &mut self,
        timeout_ms: u64,
    ) -> Option<(InteractionId, orchestration::Answer)> {
        let ask = self.head_kit_ask().filter(asks::is_question)?;
        let answer = self.question_value(&ask, Some(timeout_ms))?;
        let id = ui_id(self)?;
        self.asks.answered.insert(id.clone());
        Some((id, answer))
    }

    /// 回答被拒绝（runner 的回执）：说出原因，回答重新可选（请求还在视图里）。
    pub fn answer_failed(&mut self, ask: &str, reason: &str) {
        self.said.failed = Some(words::could_not_answer(self.lang, reason));
        self.asks.answered.retain(|id| id.as_str() != ask);
    }
}

fn ui_id(app: &App) -> Option<InteractionId> {
    app.interaction.as_ref().map(|ui| ui.id.clone())
}
