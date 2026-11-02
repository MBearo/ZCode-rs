//! 输入能力的界面文字（docs/specs/rust-cli/tui-input.md）：中文与英文。英文照原文。
use super::i18n::Lang;

/// 暂存后输入框上方的提示。
pub fn stashed(lang: Lang) -> &'static str {
    lang.pick(
        "\u{203a} Stashed (auto-restores after submit)",
        "\u{203a} 已暂存（发送后自动放回）",
    )
}

/// 排队的插话下面那一行立即发送的提示（修复线 F104，tui.md §5.1.1）：`key` 是当前终端按得出来的键。整行同一种样式
/// （F105），不再用按键提示组件的 `{键}` 标记。
pub fn send_now_hint(lang: Lang, key: &str) -> String {
    match lang {
        Lang::En => format!("{key} to send now"),
        Lang::Zh => format!("{key} 立即发送"),
    }
}

/// 发送后放回暂存的提示。
pub fn draft_restored(lang: Lang) -> &'static str {
    lang.pick("Draft restored", "已放回暂存的输入")
}

/// `/help` 里快捷键一节的标题。
pub fn shortcuts_title(lang: Lang) -> &'static str {
    lang.pick("Shortcuts", "快捷键")
}

/// bash 模式的输入框提示。
pub fn bash_placeholder(lang: Lang) -> &'static str {
    lang.pick("Run a shell command", "运行 shell 命令")
}

/// 剪贴板里没有图片（原文，`key` 是粘贴图片的键）。
pub fn no_image(lang: Lang, key: &str) -> String {
    match lang {
        Lang::En => format!("No image found in clipboard. Use {key} to paste images."),
        Lang::Zh => format!("剪贴板里没有图片。用 {key} 粘贴图片。"),
    }
}

/// 图片超过上限（不缩放，D10）。
pub fn image_too_large(lang: Lang) -> String {
    match lang {
        Lang::En => "Image is too large to attach (over 3.75MB).".to_owned(),
        Lang::Zh => "图片太大，不能附上（超过 3.75MB）。".to_owned(),
    }
}

/// 读剪贴板失败（原因原样）。
pub fn clipboard_failed(lang: Lang, reason: &str) -> String {
    match lang {
        Lang::En => format!("Could not read the clipboard: {reason}"),
        Lang::Zh => format!("读不了剪贴板：{reason}"),
    }
}

/// 编辑器失败（`<名字> exited with code N` 等，原文原样）。
pub fn editor_failed(lang: Lang, reason: &str) -> String {
    match lang {
        Lang::En => reason.to_owned(),
        Lang::Zh => format!("外部编辑器失败：{reason}"),
    }
}

/// `/memory` 选择器的标题与说明。
pub fn memory_title(lang: Lang) -> &'static str {
    lang.pick("Memory", "记忆")
}

pub fn memory_subtitle(lang: Lang) -> &'static str {
    lang.pick(
        "Select a memory file to open in your editor.",
        "选择要用编辑器打开的记忆文件。",
    )
}

/// `/memory` 按 esc（原文）。
pub fn memory_cancelled(lang: Lang) -> &'static str {
    lang.pick("Cancelled memory editing", "已取消编辑记忆")
}
