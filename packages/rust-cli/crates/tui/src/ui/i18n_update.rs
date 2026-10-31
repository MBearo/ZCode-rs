//! 自动更新的文字（docs/specs/rust-cli/update.md §6.1、§7）：英文与中文。
use super::i18n::Lang;

pub fn title(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Update available",
        Lang::Zh => "有新版本",
    }
}

pub fn release_notes(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Release notes: ",
        Lang::Zh => "更新说明：",
    }
}

/// 三个选项：立即更新、跳过、跳过直到下一个版本。
pub fn options(lang: Lang, command: &str) -> [String; 3] {
    match lang {
        Lang::En => [
            format!("Update now (runs `{command}`)"),
            "Skip".to_owned(),
            "Skip until next version".to_owned(),
        ],
        Lang::Zh => [
            format!("立即更新（运行 `{command}`）"),
            "跳过".to_owned(),
            "跳过，直到下一个版本".to_owned(),
        ],
    }
}

/// 按键提示（按键提示组件的模板）。
pub fn keys_hint(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "{enter} continue · {esc} skip",
        Lang::Zh => "{enter} 继续 · {esc} 跳过",
    }
}

/// 会话里的提示：标题。
pub fn notice_title(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Update available!",
        Lang::Zh => "有新版本！",
    }
}

/// 会话里的提示：`Run <命令> to update.` 拆成命令前后两段。
pub fn notice_run(lang: Lang) -> (&'static str, &'static str) {
    match lang {
        Lang::En => ("Run ", " to update."),
        Lang::Zh => ("运行 ", " 更新。"),
    }
}

pub fn notice_notes(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "See full release notes:",
        Lang::Zh => "完整的更新说明：",
    }
}
