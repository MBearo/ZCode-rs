//! 工作区信任对话框的文字（docs/specs/rust-cli/workspace-trust.md §2.4）：英文照原文
//! （产品名换成 ZCode），另配中文。
use super::i18n::Lang;

pub fn title(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Accessing workspace:",
        Lang::Zh => "访问工作区：",
    }
}

pub fn safety_check(lang: Lang) -> &'static str {
    match lang {
        Lang::En => {
            "Quick safety check: Is this a project you created or one you trust? (Like your own code, a well-known open source project, or work from your team). If not, take a moment to review what's in this folder first."
        }
        Lang::Zh => {
            "快速安全检查：这是你自己创建或信任的项目吗？（例如你自己的代码、知名的开源项目，或团队的工作。）如果不是，请先花点时间看看这个文件夹里有什么。"
        }
    }
}

pub fn abilities(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "ZCode'll be able to read, edit, and execute files here.",
        Lang::Zh => "ZCode 将可以读取、编辑和执行这里的文件。",
    }
}

/// 项目授权的标题（`This folder pre-approves <n> <tool permission(s)> in <来源>:`）。
pub fn grants_title(lang: Lang, count: usize, sources: &str) -> String {
    match lang {
        Lang::En => {
            let noun = if count == 1 {
                "tool permission"
            } else {
                "tool permissions"
            };
            format!("This folder pre-approves {count} {noun} in {sources}:")
        }
        Lang::Zh => format!("此文件夹在 {sources} 中预先批准了 {count} 项工具权限："),
    }
}

pub fn grants_note(lang: Lang) -> &'static str {
    match lang {
        Lang::En => {
            "These will apply without asking. Only proceed if you trust this configuration."
        }
        Lang::Zh => "这些权限会直接生效，不再询问。只有信任这份配置时才继续。",
    }
}

pub fn confirm(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Yes, I trust this folder",
        Lang::Zh => "是，我信任此文件夹",
    }
}

pub fn cancel(lang: Lang, backstop: bool) -> &'static str {
    match (lang, backstop) {
        (Lang::En, false) => "No, exit",
        (Lang::En, true) => "No, continue without these permissions",
        (Lang::Zh, false) => "否，退出",
        (Lang::Zh, true) => "否，不带这些权限继续",
    }
}

pub fn keys_hint(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "{Enter} to confirm · {Esc} to cancel",
        Lang::Zh => "{Enter} 确认 · {Esc} 取消",
    }
}

/// 列表的写法：两项用 ` and `，三项以上用 `, ` 与 `, and `；超过 `limit` 时只列前 `limit` 项，
/// 其余写 `and <k> more`。中文用 `、` 与 `和`。
pub fn list(lang: Lang, items: &[String], limit: Option<usize>) -> String {
    let shown = limit.filter(|limit| items.len() > *limit);
    let head = &items[..shown.unwrap_or(items.len())];
    match (lang, shown, items) {
        (_, None, [only]) => only.clone(),
        (Lang::En, None, [first, second]) => format!("{first} and {second}"),
        (Lang::En, None, _) => match items.split_last() {
            Some((last, rest)) => format!("{}, and {last}", rest.join(", ")),
            None => String::new(),
        },
        (Lang::En, Some(_), _) => {
            let more = items.len() - head.len();
            if head.len() == 1 {
                format!("{} and {more} more", head[0])
            } else {
                format!("{}, and {more} more", head.join(", "))
            }
        }
        (Lang::Zh, None, _) => match items.split_last() {
            Some((last, rest)) if !rest.is_empty() => format!("{} 和 {last}", rest.join("、")),
            _ => String::new(),
        },
        (Lang::Zh, Some(_), _) => format!(
            "{} 以及另外 {} 项",
            head.join("、"),
            items.len() - head.len()
        ),
    }
}
