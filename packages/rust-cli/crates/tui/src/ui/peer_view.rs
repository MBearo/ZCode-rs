//! 扣住的同伴消息的批准（docs/specs/rust-cli/cross-session.md §2.6）：发送方、原因、正文，两个回答（缺省焦点在投递：Puffin 的请求都选中第一个回答）。内容
//! 只来自会话视图的 `peer_held`。
//!
//! Puffin 没有这一面（旧界面多出的能力，tui-kit.md §4.3「同伴消息的批准」），照 Puffin 最近的样子画：没有工具行的请求
//! （Puffin 子代理的请求）。板的顶上是请求的行（kit `head_row_said`、右边 `N more waiting`、编号的 `answer_rows`，打开后
//! [`ARM_TICKS`] 内淡入），输入框照常在，槽是 `1–2 answer  esc deny`；消息在正文末尾，`● Peer  名字` 一行与轨上的发送方、
//! 原因与全文（Puffin 的预览不截断，`ask_body.rs`）。键照 Puffin 的请求：↑↓ ^P ^N 移动，数字直接回答，⏎ 回答选中的，
//! esc 拒绝（这一面不停回合：扣住的消息与回合无关）。以前是板上面的轨，正文截到 8 行、72 列，槽是旧的提示句。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_kernel_api::peers::HeldPeerMessage;
use rust_cli_tui_kit::app::element::{Element, Wrap, single_line};
use rust_cli_tui_kit::app::widgets::asks::data::AskOptionKind;
use rust_cli_tui_kit::app::widgets::asks::{self, ARM_TICKS, Answer, AnswerKind};
use rust_cli_tui_kit::render::{Span, Style, StyledLine, attr};

use super::app::{Action, App};
use super::i18n_asks;
use super::i18n_peers as words;
use super::interaction::ticks_since;
use super::interaction_view::board::{Laid, lay_all, rail_line, slot_pairs};
use super::interaction_view::{At, Rows};
use super::keyboard;

/// 回答的下标：投递在前（缺省焦点，同 Puffin 的请求选中第一个回答），拒绝在后。
const DELIVER: usize = 0;
const DENY: usize = 1;

impl App {
    /// 在批准的同伴消息：没有请求开着时最早扣住的那一条。
    pub fn peer_asking(&self) -> Option<&HeldPeerMessage> {
        if self.pending().is_some() {
            return None;
        }
        self.view.peer_held()
    }

    /// 这一条点亮了多少（Puffin 请求的 `armedness`）。
    pub(super) fn peer_armedness(&self, now: Instant) -> f64 {
        let Some(held) = self.peer_asking() else {
            return 0.0;
        };
        match &self.asks.peer {
            Some((id, at)) if *id == held.msg_id => {
                (ticks_since(*at, now) as f64 / ARM_TICKS as f64).min(1.0)
            }
            _ => 0.0,
        }
    }

    /// 记下扣住的消息第一次在批准的时刻；换了一条时选择回到第一个回答（投递，Puffin 的请求的缺省）。
    pub(super) fn sync_peer(&mut self, now: Instant) {
        let id = self.peer_asking().map(|held| held.msg_id.clone());
        match (id, &self.asks.peer) {
            (None, _) => self.asks.peer = None,
            (Some(id), Some((seen, _))) if *seen == id => {}
            (Some(id), _) => {
                self.asks.peer = Some((id, now));
                self.peer_choice = DELIVER;
            }
        }
    }

    /// 两个回答：投递（缺省焦点，[`DELIVER`]）与拒绝，名字与 fg3 的说明（字照界面语言）。
    fn peer_answers(&self) -> Vec<Answer> {
        let fg3 = self.ink.theme.fg3;
        let [(deliver, why_deliver), (deny, why_deny)] = words::options(self.lang);
        let answer = |id: &str, kind, label: &str, desc: &str| Answer {
            id: id.to_owned(),
            kind,
            label: label.to_owned(),
            desc: vec![(desc.to_owned(), fg3)],
        };
        vec![
            answer("deliver", AnswerKind::Approve, deliver, why_deliver),
            answer(
                "deny",
                AnswerKind::Option(AskOptionKind::Deny),
                deny,
                why_deny,
            ),
        ]
    }

    /// 板上的一块：板头、编号的回答；草稿行照常，槽是这一面的键。
    pub fn peer_laid(&self, width: usize, now: Instant) -> Option<(Laid, Rows, At)> {
        self.peer_asking()?;
        let theme = &self.ink.theme;
        let lang = self.lang;
        let more = i18n_asks::more_waiting(lang, self.view.peer_held_count().saturating_sub(1));
        let mut rows = vec![asks::head_row_said(
            theme,
            words::title(lang),
            None,
            more.as_deref(),
        )];
        let answers = self.peer_answers();
        rows.extend(asks::answer_rows(
            theme,
            &answers,
            self.peer_choice,
            self.peer_armedness(now),
        ));
        let lines = lay_all(&rows, width);
        let first = lines.len().saturating_sub(answers.len());
        let options = (0..answers.len()).map(|i| (first + i, i)).collect();
        let pair = |k: &str, v: &str| (k.to_owned(), i18n_asks::slot(lang, v).to_owned());
        let slot = slot_pairs(theme, &[pair("1–2", "answer"), pair("esc", "deny")]);
        let laid = Laid {
            input_at: lines.len(),
            lines,
            slot,
            ..Laid::default()
        };
        Some((laid, options, None))
    }

    /// 批准开着时的键；`None` 把键传下去（打字、滚动、换模式）。
    pub(super) fn peer_key(&mut self, key: KeyEvent, now: Instant) -> Option<Option<Action>> {
        let message = self.peer_asking()?.msg_id.clone();
        let ctrl = keyboard::ctrl(&key);
        let armed = self.peer_armedness(now) >= 1.0;
        let empty = self.composer.area.text().is_empty();
        let answer = |approve: bool| Action::ResolvePeer {
            message: message.clone(),
            approve,
        };
        Some(match key.code {
            KeyCode::Up => {
                self.peer_choice = DELIVER;
                None
            }
            KeyCode::Char('p') if ctrl => {
                self.peer_choice = DELIVER;
                None
            }
            KeyCode::Down => {
                self.peer_choice = DENY;
                None
            }
            KeyCode::Char('n') if ctrl => {
                self.peer_choice = DENY;
                None
            }
            KeyCode::Char(c @ ('1' | '2')) if empty && !ctrl => armed.then(|| answer(c == '1')),
            KeyCode::Enter => armed.then(|| answer(self.peer_choice == DELIVER)),
            KeyCode::Esc => Some(answer(false)),
            _ => return None,
        })
    }
}

/// 正文末尾的消息（Puffin 没有行的请求的 `own` 与预览）：`● Peer  名字`，轨上的发送方与原因（fg3）、全文。
pub fn preview(app: &App, width: usize) -> Option<Vec<StyledLine>> {
    let held = app.peer_asking()?;
    let theme = &app.ink.theme;
    let lang = app.lang;
    let who = held
        .from_name
        .clone()
        .unwrap_or_else(|| held.from.as_str().to_owned());
    let mut rows: Vec<Element> = vec![single_line(
        vec![
            Span::plain(" "),
            Span::new("●", Style::fg(theme.code)),
            Span::plain(" "),
            Span::new("Peer", theme.fg2.with_attr(attr::BOLD)),
            Span::plain("  "),
            Span::new(who, theme.fg3),
        ],
        false,
    )];
    let from = words::from(lang, held.from.as_str(), held.from_name.as_deref());
    rows.push(rail_line(theme, &from, theme.fg3, Wrap::Word));
    rows.push(rail_line(
        theme,
        words::cause(lang, held.cause),
        theme.fg3,
        Wrap::Word,
    ));
    if !held.body.is_empty() {
        rows.extend(
            held.body
                .lines()
                .map(|line| rail_line(theme, line, Style::DEFAULT, Wrap::Char)),
        );
    }
    Some(lay_all(&rows, width))
}
