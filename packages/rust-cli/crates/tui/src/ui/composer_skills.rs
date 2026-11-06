//! `$` 面板的状态（tui-input.md §1.4；Puffin `MentionUi`、`settle_mention`、`mention_key` 的插入）：目录、esc 为哪个 `$`
//! 关上了面板、各种淡入开始的 tick。tick 在画的时候记（`skill_view.rs` 的 `note_skills`）：按键只改状态，画面按这一帧的
//! tick 判断什么刚刚变了。
use super::{Composer, InputMode, Popup};
use crate::ui::skill_mention::{self, MentionSkill};

/// 很久以前（同 Puffin 状态的 `LONG_AGO`）：从这时起的淡入早已落定。
pub const LONG_AGO: i64 = -1_000_000;

#[derive(Debug, Clone)]
pub struct Mentions {
    /// 技能的目录（[`skill_mention::catalog`] 折叠、排序过）；还没到时 None（面板说 `Looking for skills…`）。
    pub catalog: Option<Vec<MentionSkill>>,
    /// esc 为这个字节处的 `$` 关上了面板；没有 `$` 词时忘掉。
    pub closed_at: Option<usize>,
    /// 上一次的 `$` 词：换了时选第一行。
    word: Option<(usize, String)>,
    pub opened_at: i64,
    pub shown_at: i64,
    pub inserted_at: i64,
    /// 刚插入的提及在草稿里的范围（淡入）。
    pub inserted: (usize, usize),
    /// 插入了、还没画：这一帧的 tick 是它的 `inserted_at`。
    pub just_inserted: bool,
    /// 上一帧：面板开着、预览的技能。
    pub seen: (bool, Option<String>),
}

impl Default for Mentions {
    fn default() -> Self {
        Self {
            catalog: None,
            closed_at: None,
            word: None,
            opened_at: LONG_AGO,
            shown_at: LONG_AGO,
            inserted_at: LONG_AGO,
            inserted: (0, 0),
            just_inserted: false,
            seen: (false, None),
        }
    }
}

impl Composer {
    /// 光标前的 `$` 词；bash 模式下 `$` 是 shell 的（Puffin `mention_token`）。
    pub fn skill_token(&self) -> Option<(usize, String)> {
        if self.mode == InputMode::Bash {
            return None;
        }
        skill_mention::token(self.area.text(), self.area.cursor())
    }

    /// 一个键之后（`refresh_popup`，在 `/` 命令架之后、`@` 之前）：有 `$` 词时面板开着（esc 为它关上了时不开），词变了选
    /// 第一行；返回有没有 `$` 词。
    pub(super) fn refresh_skills(&mut self) -> bool {
        let Some(word) = self.skill_token() else {
            self.mentions.closed_at = None;
            self.mentions.word = None;
            return false;
        };
        let same = self.mentions.word.as_ref() == Some(&word);
        let at = word.0;
        self.mentions.word = Some(word);
        if self.mentions.closed_at == Some(at) {
            self.popup = None;
            return true;
        }
        let selected = match &self.popup {
            Some(Popup::Skills { selected }) if same => *selected,
            _ => 0,
        };
        self.popup = Some(Popup::Skills { selected });
        true
    }

    /// 面板为现在的词找到的技能，好的在前。
    pub fn skill_matches(&self) -> Vec<&MentionSkill> {
        let q = self.skill_token().map(|(_, q)| q).unwrap_or_default();
        skill_mention::matches(self.mentions.catalog.as_deref().unwrap_or(&[]), &q)
    }

    /// ⇥ / ⏎：`$` 词换成 `$名字`（后面没有空白时加一个空格），光标停在空格之后，面板关上（Puffin `mention_key`）。两者都
    /// 不发送。
    pub fn insert_skill(&mut self, name: &str) {
        let Some((start, _)) = self.skill_token() else {
            return;
        };
        let text = self.area.text().to_owned();
        let ((from, to), with, range) =
            skill_mention::insert(&text, self.area.cursor(), start, name);
        self.area.replace_range(from, to, &with);
        let stop = start + 1 + name.len();
        if self.area.text().as_bytes().get(stop) == Some(&b' ') {
            self.area.set_cursor(stop + 1);
        }
        self.mentions.inserted = range;
        self.mentions.just_inserted = true;
        self.refresh_popup();
    }
}
