//! 暂存输入（ctrl+s，docs/specs/rust-cli/tui-input.md §5）：整份草稿（文字、光标、
//! 粘贴的折叠内容）放到一边，输入框清空；下一次发送普通输入后自动放回。
use super::Composer;

/// 一份草稿。
pub struct Draft {
    text: String,
    cursor: usize,
    pastes: Vec<(String, String)>,
    mode: super::InputMode,
    images: Vec<super::images::Pasted>,
}

impl Composer {
    pub fn has_stash(&self) -> bool {
        self.stash.is_some()
    }

    /// ctrl+s：输入不为空时暂存并清空（返回真）；为空且有暂存时取回。
    pub fn toggle_stash(&mut self) -> bool {
        if !self.area.text().trim().is_empty() {
            self.stash = Some(Draft {
                text: self.area.text().to_owned(),
                cursor: self.area.cursor(),
                pastes: std::mem::take(&mut self.pastes),
                mode: self.mode,
                images: std::mem::take(&mut self.images),
            });
            self.clear();
            return true;
        }
        self.pop_stash();
        false
    }

    /// 输入框为空时放回暂存的草稿；放回了返回真。
    pub fn pop_stash(&mut self) -> bool {
        if !self.area.text().trim().is_empty() {
            return false;
        }
        let Some(draft) = self.stash.take() else {
            return false;
        };
        self.clear();
        self.area.set_text(&draft.text);
        self.area.set_cursor(draft.cursor);
        self.pastes = draft.pastes;
        self.mode = draft.mode;
        self.images = draft.images;
        self.refresh_popup();
        true
    }
}

impl Composer {
    /// 交给外部编辑器的文字：折叠的粘贴展开（tui-input.md §4.1）。图片的
    /// `[Image #N]` 留着。
    pub fn expanded_text(&self) -> String {
        let mut text = self.area.text().to_owned();
        for (placeholder, content) in &self.pastes {
            text = text.replacen(placeholder.as_str(), content, 1);
        }
        text
    }

    /// 编辑器写回的文字：去掉单个末尾换行（以空行结尾时不去）；原样出现的粘贴内容
    /// 折回占位文字（`recollapsePastedContent`），没出现的不再保留。模式与图片不变。
    /// 这些之前先统一换行（与 [`Composer::paste_text`] 相同，修复线 F65）。
    pub fn from_editor(&mut self, edited: &str) {
        // 缺陷修复（F65 的 T4）：Windows 的编辑器常写 CRLF，以前 `\r` 留在输入里发给模型；折叠的粘贴存的是统一过换行
        // 的原文，带 `\r` 的读回也折不回去。照粘贴的做法先统一。
        let edited = edited.replace("\r\n", "\n").replace('\r', "\n");
        let edited = match edited.strip_suffix('\n') {
            Some(rest) if !edited.ends_with("\n\n") => rest,
            _ => edited.as_str(),
        };
        let mut text = edited.to_owned();
        let mut kept = Vec::new();
        for (placeholder, content) in std::mem::take(&mut self.pastes) {
            if let Some(at) = text.find(&content) {
                text.replace_range(at..at + content.len(), &placeholder);
                kept.push((placeholder, content));
            }
        }
        self.pastes = kept;
        self.area.set_text(&text);
        self.refresh_popup();
    }
}

#[cfg(test)]
#[path = "composer_draft_tests.rs"]
mod tests;
