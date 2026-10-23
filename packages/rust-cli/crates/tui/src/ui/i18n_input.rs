//! 输入能力的界面文字（docs/specs/rust-cli/tui-input.md）：中文与英文。英文照原文。
use super::i18n::Lang;

fn pick(lang: Lang, en: &'static str, zh: &'static str) -> &'static str {
    match lang {
        Lang::En => en,
        Lang::Zh => zh,
    }
}

/// 暂存后输入框上方的提示。
pub fn stashed(lang: Lang) -> &'static str {
    pick(
        lang,
        "\u{203a} Stashed (auto-restores after submit)",
        "\u{203a} 已暂存（发送后自动放回）",
    )
}

/// 发送后放回暂存的提示。
pub fn draft_restored(lang: Lang) -> &'static str {
    pick(lang, "Draft restored", "已放回暂存的输入")
}

/// `/help` 里快捷键一节的标题。
pub fn shortcuts_title(lang: Lang) -> &'static str {
    pick(lang, "Shortcuts", "快捷键")
}

/// bash 模式的输入框提示。
pub fn bash_placeholder(lang: Lang) -> &'static str {
    pick(lang, "Run a shell command", "运行 shell 命令")
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

/// 没有注入剪贴板服务。
pub fn clipboard_unavailable(lang: Lang) -> &'static str {
    pick(
        lang,
        "Image paste is not available here.",
        "这里不能粘贴图片。",
    )
}

/// 没有注入编辑器服务。
pub fn editor_unavailable(lang: Lang) -> &'static str {
    pick(
        lang,
        "No external editor is available. Set $EDITOR or $VISUAL.",
        "没有可用的外部编辑器。请设置 $EDITOR 或 $VISUAL。",
    )
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
    pick(lang, "Memory", "记忆")
}

pub fn memory_subtitle(lang: Lang) -> &'static str {
    pick(
        lang,
        "Select a memory file to open in your editor.",
        "选择要用编辑器打开的记忆文件。",
    )
}

/// `/memory` 按 esc（原文）。
pub fn memory_cancelled(lang: Lang) -> &'static str {
    pick(lang, "Cancelled memory editing", "已取消编辑记忆")
}
