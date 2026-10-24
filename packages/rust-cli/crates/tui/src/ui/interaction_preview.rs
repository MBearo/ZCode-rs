//! 带预览的题的按键（docs/specs/rust-cli/tools-more.md §1.1，工作线 F72）：↑ / ↓ 与数字只移动焦点，enter 选焦点所在的
//! 选项，`n` 写备注（esc 停止，enter 提交），esc 取消。没有 `Other` 行。
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;

use super::{Interaction, Question};
use crate::ui::keyboard::Typing;

/// 写了备注、没有选时的答案（结果里写成没有选）。
const NOTES_ONLY: &str = "(notes only)";

impl Interaction {
    /// 翻到一道题：预览题的焦点放在选过的选项上，焦点算作没动过。
    pub(super) fn enter_question(&mut self, list: &[Question]) {
        self.cursor = list
            .get(self.question)
            .filter(|question| question.preview_mode())
            .and_then(|question| {
                let chosen = self.drafts[self.question].values.first()?;
                question
                    .options
                    .iter()
                    .position(|(label, _)| label == chosen)
            })
            .unwrap_or(0);
        self.moved = false;
    }

    fn focus(&mut self, index: usize) {
        if index != self.cursor {
            self.moved = true;
        }
        self.cursor = index;
    }

    /// 选焦点所在的选项：一题时提交，多题时到下一题。
    fn choose(&mut self, list: &[Question], index: usize) -> Option<Value> {
        let (label, _) = list[self.question].options.get(index)?;
        self.drafts[self.question].values = vec![label.clone()];
        self.advance(list)
    }

    pub(super) fn preview_key(
        &mut self,
        list: &[Question],
        key: KeyEvent,
        input: Typing,
    ) -> Option<Value> {
        let count = list[self.question].options.len();
        if self.typing {
            let draft = &mut self.drafts[self.question];
            match key.code {
                KeyCode::Esc => self.typing = false,
                KeyCode::Backspace => {
                    draft.notes.pop();
                }
                KeyCode::Enter => {
                    self.typing = false;
                    let noted = !draft.notes.trim().is_empty();
                    if noted && self.moved {
                        return self.choose(list, self.cursor);
                    }
                    if !draft.values.is_empty() {
                        return self.advance(list);
                    }
                    if noted {
                        draft.values = vec![NOTES_ONLY.to_owned()];
                        return self.advance(list);
                    }
                }
                KeyCode::Char(c) if input.is_text(&key) => draft.notes.push(c),
                _ => {}
            }
            return None;
        }
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Up if self.cursor > 0 => self.focus(self.cursor - 1),
            KeyCode::Down if self.cursor + 1 < count => self.focus(self.cursor + 1),
            KeyCode::Enter => return self.choose(list, self.cursor),
            KeyCode::Esc => return self.decline(),
            KeyCode::Char('n') if plain => self.typing = true,
            KeyCode::Char(c @ '1'..='9') => {
                let index = c as usize - '1' as usize;
                if index < count {
                    self.focus(index);
                }
            }
            _ => {}
        }
        None
    }
}

#[cfg(test)]
#[path = "interaction_preview_tests.rs"]
mod tests;
