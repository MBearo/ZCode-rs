//! esc 撤回刚发的输入（修复线 F38，docs/specs/rust-cli/tui-input.md §8）：内核回退对话后，
//! 把这条输入放回输入框。
use super::Composer;
use super::images::Pasted;

/// 文字里的 `[Image #N]`（按出现顺序，去重）。
fn image_ids(text: &str) -> Vec<usize> {
    let mut ids = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("[Image #") {
        rest = &rest[at + "[Image #".len()..];
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits > 0
            && rest[digits..].starts_with(']')
            && let Ok(id) = rest[..digits].parse::<usize>()
            && !ids.contains(&id)
        {
            ids.push(id);
        }
    }
    ids
}

impl Composer {
    /// 放回撤回的输入：文字（`!` 开头回到 bash 模式），光标在末尾。`images` 是消息里
    /// 的图片块（`(media_type, base64)`，按发送时的顺序，即编号从小到大），按编号配给文字里的 `[Image #N]`，之后的粘贴编号
    /// 接着往后排。输入框里已经有字时（判断之后、回执之前键入的，同步执行时没有
    /// 这个空档）接在放回的文字后面一行，不丢。
    pub fn restore_input(&mut self, text: &str, images: Vec<(String, String)>) {
        let typed = self.area.text().to_owned();
        let combined = if typed.trim().is_empty() {
            text.to_owned()
        } else {
            format!("{text}\n{typed}")
        };
        self.clear();
        self.load_entry(&combined);
        let mut ids = image_ids(text);
        ids.sort_unstable();
        for (id, (media_type, data)) in ids.into_iter().zip(images) {
            self.paste_count = self.paste_count.max(id);
            self.images.push(Pasted {
                id,
                placeholder: format!("[Image #{id}]"),
                media_type,
                data,
            });
        }
    }
}

#[cfg(test)]
#[path = "composer_restore_tests.rs"]
mod tests;
