//! 输入能力的界面文字（docs/specs/rust-cli/tui-input.md）：中文与英文。英文照原文。
use super::i18n::Lang;

/// 暂存后输入框上方的提示。
pub fn stashed(lang: Lang) -> &'static str {
    lang.pick(
        "\u{203a} Stashed (auto-restores after submit)",
        "\u{203a} 已暂存（发送后自动放回）",
    )
}

/// 发送后放回暂存的提示。
pub fn draft_restored(lang: Lang) -> &'static str {
    lang.pick("Draft restored", "已放回暂存的输入")
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

/// 排队的消息多于四条时栈最上面一行（docs/puffin-tui.md §4.2「发出的消息」的「栈」）：看不见的条数。
pub fn earlier(lang: Lang, n: usize) -> String {
    match lang {
        Lang::En => format!("{n} earlier"),
        Lang::Zh => format!("更早的 {n} 条"),
    }
}

/// 栈最上面一行在槽的位置的说明（Puffin `STACK_LABEL`）：这些消息等下一轮。
pub fn next_turn(lang: Lang) -> &'static str {
    lang.pick("next turn", "下一轮")
}

/// 第一次进 shell 模式时输入框上方的一句（Puffin `shell_view::note`）。
pub fn shell_note(lang: Lang, shell: &str) -> String {
    match lang {
        Lang::En => {
            format!(
                "Shell: {shell}, in this folder. The agent sees the output with your next message."
            )
        }
        Lang::Zh => format!("Shell：{shell}，在这个文件夹里。下一条消息时 agent 会看到输出。"),
    }
}
