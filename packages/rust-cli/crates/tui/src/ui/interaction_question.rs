// 改写自 apps/zcode-host/crates/tui/src/ui/interaction_question.rs（feat/rs 79b6691c55）：去掉旧式 userInput。
//! 提问的按键：多于一题时有标签页与确认页。选项之后是 `Other` 行（直接输入）；单选题选中即到下一题，
//! 多选题空格或数字勾选；←/→ 或 tab 切换标签页；esc 拒绝回答。
use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_kernel_api::orchestration::InteractionRequest;
use serde_json::Value;

use super::{Interaction, Question, questions};
use crate::ui::keyboard::Typing;

fn toggle(values: &mut Vec<String>, value: &str) {
    match values.iter().position(|v| v == value) {
        Some(index) => {
            values.remove(index);
        }
        None => values.push(value.to_owned()),
    }
}

/// 题目的行数：选项加 `Other` 行。
pub(in crate::ui) fn rows(question: &Question) -> usize {
    question.options.len() + 1
}

impl Interaction {
    pub(super) fn question_key(
        &mut self,
        request: &InteractionRequest,
        key: KeyEvent,
        input: Typing,
    ) -> Option<Value> {
        let list = questions(request);
        let pages = if list.len() > 1 { list.len() + 1 } else { 1 };
        let typed = input.is_text(&key);
        let preview = list.get(self.question).is_some_and(Question::preview_mode);
        // 预览题写备注时 tab 与左右键不切换题目（工作线 F72）。
        if preview && self.typing {
            return self.preview_key(&list, key, input);
        }
        let switch = |this: &mut Self, page: usize| {
            this.question = page;
            this.typing = false;
            this.enter_question(&list);
        };
        match key.code {
            KeyCode::Left if self.question > 0 => {
                switch(self, self.question - 1);
                return None;
            }
            KeyCode::Right | KeyCode::Tab if self.question + 1 < pages => {
                switch(self, self.question + 1);
                return None;
            }
            KeyCode::BackTab if self.question > 0 => {
                switch(self, self.question - 1);
                return None;
            }
            _ => {}
        }
        if preview {
            return self.preview_key(&list, key, input);
        }
        // 确认页：1. Submit answers；2. Cancel。
        let Some(question) = list.get(self.question) else {
            return match key.code {
                KeyCode::Up | KeyCode::Down => {
                    self.cursor = 1 - self.cursor.min(1);
                    None
                }
                KeyCode::Char('1') => self.submit(&list),
                KeyCode::Enter if self.cursor == 0 => self.submit(&list),
                KeyCode::Char('2') | KeyCode::Enter | KeyCode::Esc => self.decline(),
                _ => None,
            };
        };
        let count = rows(question);
        let other = question.options.len();
        if self.typing {
            let draft = &mut self.drafts[self.question];
            match key.code {
                KeyCode::Esc => self.typing = false,
                KeyCode::Up => {
                    self.typing = false;
                    self.cursor = (self.cursor + count - 1) % count;
                }
                KeyCode::Down => {
                    self.typing = false;
                    self.cursor = (self.cursor + 1) % count;
                }
                KeyCode::Backspace => {
                    draft.notes.pop();
                }
                KeyCode::Enter => {
                    if !question.multi && !draft.notes.trim().is_empty() {
                        draft.values.clear();
                    }
                    return self.advance(&list);
                }
                KeyCode::Char(c) if typed => draft.notes.push(c),
                _ => {}
            }
            return None;
        }
        let draft = &mut self.drafts[self.question];
        match key.code {
            KeyCode::Up => self.cursor = (self.cursor + count - 1) % count,
            KeyCode::Down => self.cursor = (self.cursor + 1) % count,
            KeyCode::Char(' ') if question.multi => {
                if let Some((value, _)) = question.options.get(self.cursor) {
                    toggle(&mut draft.values, value);
                }
            }
            KeyCode::Char(c @ '1'..='9') => {
                let index = c as usize - '1' as usize;
                if index == other {
                    self.cursor = index;
                } else if let Some((value, _)) = question.options.get(index) {
                    self.cursor = index;
                    if question.multi {
                        toggle(&mut draft.values, value);
                    } else {
                        draft.values = vec![value.clone()];
                        draft.notes.clear();
                        return self.advance(&list);
                    }
                }
            }
            KeyCode::Enter => {
                if let Some((value, _)) = question.options.get(self.cursor) {
                    if !question.multi {
                        draft.values = vec![value.clone()];
                        draft.notes.clear();
                    } else if draft.values.is_empty() {
                        draft.values.push(value.clone());
                    }
                }
                return self.advance(&list);
            }
            KeyCode::Esc => return self.decline(),
            KeyCode::Char(c) if typed && self.cursor == other => draft.notes.push(c),
            _ => {}
        }
        // 焦点在 `Other` 行时打字直接进入回答。
        self.typing = self.cursor == other;
        None
    }

    /// 下一题；只有一题或已在最后一题时：一题直接提交，多题进入确认页。
    pub(super) fn advance(&mut self, list: &[Question]) -> Option<Value> {
        self.typing = false;
        if list.len() == 1 {
            return self.submit(list);
        }
        self.question += 1;
        self.enter_question(list);
        None
    }
}
