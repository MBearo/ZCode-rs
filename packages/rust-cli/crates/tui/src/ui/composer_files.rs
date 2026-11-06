//! `@` 面板的状态（docs/specs/rust-cli/tui-input.md §1.2；照 Puffin `shell/at.rs` 的 `AtData` 与 `shell/mentions.rs` 的
//! `MentionUi`）：输入框里的 `@` 词、esc 关上的词、最新问的搜索与它的回答、读过的预览、git 的改动与三个淡入的 tick。
//!
//! 数据流（清单的唯一来源是执行环境；这里只持有可丢弃的缓存与回答）：
//!
//! ```text
//! 按键 ──▶ refresh_files：@ 词 ─(词变了)─▶ asked、search（等 runner 取走）
//! runner ──▶ 异步：缓存的清单（没有时 Kernel::workspace_files）上 at_search::search ──▶ found（只收 asked 的回答）
//! 画之前 ──▶ App::note_at：面板打开、选中的项换了、文件晚到 → 淡入的 tick；选中的路径没读过 → peek（等 runner 取走）
//! ```
//!
//! 以前按键时在内存里的 `FileIndex` 上同步匹配，5 秒节流再取清单；回答是同步的，面板没有「在找」与预览。
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rust_cli_tui_kit::app::widgets::at_panel::data::{ConversationRef, PathPeek};
use rust_cli_tui_kit::app::widgets::at_panel::{AtItem, title_of};
use rust_cli_tui_kit::app::widgets::{fit, mention_wire};

use super::{Composer, Popup};
use crate::ui::at_panel::{AtFiles, split};
use crate::ui::at_search::FileHit;
use crate::ui::mention::{at_token, replacement};

/// 草稿里会话的标题截到这么多格（Puffin `at::LABEL_MAX`）。
pub const LABEL_MAX: usize = 32;

/// 很久以前（同 Puffin 状态的 `LONG_AGO`）：从这时起的淡入早已落定。
const LONG_AGO: i64 = -1_000_000;

pub struct Files {
    /// 清单推出的文件与文件夹（runner 取来缓存；工作区换了时整个 `Files` 丢掉）。
    pub entries: Option<Arc<Vec<FileHit>>>,
    /// 最新一次问的（筛选词，路径）：只收它的回答。
    pub asked: Option<(String, String)>,
    /// 还没发出的搜索（runner 取走）。
    pub search: Option<(String, String)>,
    /// 最近到达的回答。
    pub found: Option<AtFiles>,
    /// 面板开着时换了路径的回答到了：下一帧从这一刻淡入（Puffin `files_at`）。
    pub late: bool,
    pub files_at: i64,
    /// git 说改了没提交的（面板每次打开重读）。
    pub changed: Vec<String>,
    /// 这个工作区最近的会话（面板每次打开重读，Puffin host 的 `conversations`）；到之前 None。
    pub conversations: Option<Vec<rust_cli_tui_kit::app::widgets::at_panel::data::ConversationRef>>,
    /// 读过的预览与在读的；`peek` 是还没发出的（runner 取走）。
    pub peeks: BTreeMap<String, PathPeek>,
    pub peeking: BTreeSet<String>,
    pub peek: Option<String>,
    /// 面板这次打开了：runner 重读 git 的改动。
    pub reopened: bool,
    /// esc 关上的 `@` 词的起点；词没了才再开。
    pub closed_at: Option<usize>,
    pub opened_at: i64,
    pub shown_at: i64,
    /// 光标前的 `@` 词（`@` 之后的字）。
    pub word: Option<String>,
    /// 上一帧见到的：面板开着、选中的项，与终端的宽（键在两帧之间按它找选中的项）。
    pub(crate) seen: (bool, Option<AtItem>),
    pub(crate) width: usize,
}

impl Default for Files {
    fn default() -> Self {
        Self {
            entries: None,
            asked: None,
            search: None,
            found: None,
            late: false,
            files_at: LONG_AGO,
            changed: Vec::new(),
            conversations: None,
            peeks: BTreeMap::new(),
            peeking: BTreeSet::new(),
            peek: None,
            reopened: false,
            closed_at: None,
            opened_at: LONG_AGO,
            shown_at: LONG_AGO,
            word: None,
            seen: (false, None),
            width: 80,
        }
    }
}

impl Files {
    /// 搜索的回答到达：只收最新问的那一次；面板开着、路径换了时它从组名下淡入。
    pub fn found(&mut self, answer: AtFiles, open: bool) {
        if self.asked.as_ref() != Some(&(answer.query.clone(), answer.under.clone())) {
            return;
        }
        self.late = open && self.found.as_ref().is_none_or(|f| f.under != answer.under);
        self.found = Some(answer);
    }

    /// 一个路径的预览到达。
    pub fn peeked(&mut self, path: String, peek: PathPeek) {
        self.peeking.remove(&path);
        self.peeks.insert(path, peek);
    }
}

impl Composer {
    /// 输入变化后：光标前是 `@` 词时打开面板（esc 关上的词除外），词变了时问它的文件、选中回到第一项；返回是否处理了
    /// （不是 `@` 词时交给斜杠列表的判断）。
    pub(super) fn refresh_files(&mut self) -> bool {
        let cursor = self.area.cursor();
        let Some(token) = at_token(self.area.text(), cursor) else {
            self.files.closed_at = None;
            self.files.word = None;
            return false;
        };
        let from = (token.start + 1 + usize::from(token.quoted)).min(cursor);
        let word = self.area.text()[from..cursor]
            .trim_end_matches('"')
            .to_owned();
        let same = self.files.word.as_ref() == Some(&word);
        self.files.word = Some(word.clone());
        if self.files.closed_at == Some(token.start) {
            self.popup = None;
            return true;
        }
        let (under, q) = split(&word);
        let asked = (q, under);
        if self.files.asked.as_ref() != Some(&asked) {
            self.files.asked = Some(asked.clone());
            self.files.search = Some(asked);
        }
        let selected = match &self.popup {
            Some(Popup::Files { selected }) if same => *selected,
            _ => 0,
        };
        self.popup = Some(Popup::Files { selected });
        true
    }

    /// esc 关上 `@` 面板：这个词删掉或换了之前不再打开（Puffin `at_key` 的 esc）。
    pub fn close_files(&mut self) {
        if let Some(token) = at_token(self.area.text(), self.area.cursor()) {
            self.files.closed_at = Some(token.start);
        }
        self.popup = None;
    }

    /// ⇥ 或 ⏎ 放进选中的项：文件换成 `@路径 `（含空格时 `@"路径" `），面板关上；文件夹换成 `@路径/`，面板留着列出它里面
    /// 的项（Puffin `at_key`）。以前 ⇥ 先补到候选的公共前缀。
    pub fn insert_at(&mut self, item: &AtItem) {
        if let AtItem::Conversation(c) = item {
            self.insert_conversation(c);
            return;
        }
        let AtItem::Path { path, folder } = item else {
            return;
        };
        let Some(token) = at_token(self.area.text(), self.area.cursor()) else {
            return;
        };
        let text = if *folder {
            replacement(
                &format!("{}/", path.trim_end_matches('/')),
                token.quoted,
                false,
            )
        } else {
            replacement(path, token.quoted, true)
        };
        self.area.replace_range(token.start, token.end, &text);
        self.refresh_popup();
    }

    /// 会话：从 `@` 起的整个词换成一个整块的提及 `@标题`（标题截到 [`LABEL_MAX`] 格），后面没有空白时加一个空格，光标在空格
    /// 之后；发出时它是 `[#标题](#sess_…)`（Puffin `mention_of` 与 `Draft::mention`，tui-input.md §1.2「插入会话」）。
    fn insert_conversation(&mut self, c: &ConversationRef) {
        let Some(token) = at_token(self.area.text(), self.area.cursor()) else {
            return;
        };
        let label = format!("@{}", fit::clip(&title_of(c), LABEL_MAX));
        let text = self.area.text().to_owned();
        let rest = &text[self.area.cursor().min(text.len())..];
        let end = self.area.cursor() + rest.find(fit::java_space).unwrap_or(rest.len());
        let spaced = end < text.len();
        let stop = token.start + label.len();
        let with = if spaced { label } else { format!("{label} ") };
        self.area.replace_range(token.start, end, &with);
        if self.area.text().as_bytes().get(stop) == Some(&b' ') {
            self.area.set_cursor(stop + 1);
        }
        self.area.mark(
            token.start,
            stop,
            "conversation",
            mention_wire::session_link(&c.session_id, &c.title),
        );
        // 插入的提及从板上淡入（与 `$` 的插入同一个记号：淡入的是最近插入的那个，同 Puffin）。
        self.mentions.inserted = (token.start, if spaced { stop } else { stop + 1 });
        self.mentions.just_inserted = true;
        self.refresh_popup();
    }
}
