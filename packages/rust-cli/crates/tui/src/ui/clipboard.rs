//! 复制到剪贴板的终端一侧（修复线 F25，docs/specs/rust-cli/tui-select.md §3）：OSC 52 序列、复制后的提示与
//! 提示里“按住哪个键用终端自己的选择”。只看入口给的环境变量
//! （`Options::vars`），不做 IO；起进程写系统剪贴板的部分在入口的 `Clipboard` 服务。
use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine;

use super::i18n::Lang;
use crate::services::CopyRoute;

type Vars = BTreeMap<String, String>;

fn var<'a>(vars: &'a Vars, name: &str) -> Option<&'a str> {
    vars.get(name).map(String::as_str).filter(|v| !v.is_empty())
}

/// 终端复用器。
fn multiplexer(vars: &Vars) -> Option<&'static str> {
    if var(vars, "TMUX").is_some() {
        Some("tmux")
    } else if var(vars, "STY").is_some() {
        Some("screen")
    } else if var(vars, "ZELLIJ").is_some() {
        Some("zellij")
    } else {
        None
    }
}

/// 在 SSH 里（只看 `SSH_CONNECTION`）。
pub fn ssh(vars: &Vars) -> bool {
    var(vars, "SSH_CONNECTION").is_some()
}

/// 终端名（能从入口给的变量判断的部分，按判断的顺序）。
pub fn terminal(vars: &Vars) -> Option<String> {
    let term = var(vars, "TERM");
    if term == Some("xterm-ghostty") {
        return Some("ghostty".into());
    }
    if term.is_some_and(|t| t.contains("kitty")) {
        return Some("kitty".into());
    }
    if let Some(program) = var(vars, "TERM_PROGRAM") {
        return Some(program.into());
    }
    let by_var = [
        ("TMUX", "tmux"),
        ("STY", "screen"),
        ("KONSOLE_VERSION", "konsole"),
        ("GNOME_TERMINAL_SERVICE", "gnome-terminal"),
        ("XTERM_VERSION", "xterm"),
        ("VTE_VERSION", "vte-based"),
        ("KITTY_WINDOW_ID", "kitty"),
        ("ALACRITTY_LOG", "alacritty"),
        ("WT_SESSION", "windows-terminal"),
    ];
    if let Some((_, name)) = by_var.iter().find(|(key, _)| var(vars, key).is_some()) {
        return Some((*name).into());
    }
    term.map(|t| {
        if t.contains("alacritty") {
            "alacritty".into()
        } else {
            t.into()
        }
    })
}

/// OSC 52 序列：tmux 里原样一份再加一份 DCS 透传；screen 里按 76 个字符分段的 DCS；kitty 用
/// ST（`ESC \`）结尾，其余用 BEL。
pub fn osc52(text: &str, vars: &Vars) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    match multiplexer(vars) {
        Some("tmux") => {
            let raw = format!("\x1b]52;c;{encoded}\x07");
            let wrapped = format!("\x1bPtmux;{}\x1b\\", raw.replace('\x1b', "\x1b\x1b"));
            format!("{raw}{wrapped}")
        }
        Some("screen") => {
            let chunks: Vec<&str> = encoded
                .as_bytes()
                .chunks(76)
                .map(|chunk| std::str::from_utf8(chunk).unwrap_or_default())
                .collect();
            format!("\x1bP\x1b]52;c;{}\x07\x1b\\", chunks.join("\x1b\\\x1bP"))
        }
        _ if terminal(vars).as_deref() == Some("kitty") => {
            format!("\x1b]52;c;{encoded}\x1b\\")
        }
        _ => format!("\x1b]52;c;{encoded}\x07"),
    }
}

/// 没有注入剪贴板服务时的途径（测试）：只有 OSC 52。
pub const NO_SERVICE: CopyRoute = CopyRoute::Osc52;

/// 提示里按住的键。
pub fn native_key(vars: &Vars, macos: bool) -> &'static str {
    let name = terminal(vars);
    let name = name.as_deref();
    let vscode_like = ["vscode", "cursor", "windsurf", "antigravity", "codium"];
    let shift_terminals = [
        "ghostty",
        "kitty",
        "WezTerm",
        "alacritty",
        "xterm",
        "gnome-terminal",
        "vte-based",
        "konsole",
        "windows-terminal",
        "mintty",
    ];
    match name {
        Some("Apple_Terminal") => "Fn",
        Some("iTerm.app") => "Option",
        Some(n) if vscode_like.contains(&n) && macos => "Option",
        Some(n) if vscode_like.contains(&n) => "Shift",
        Some(n) if shift_terminals.contains(&n) => "Shift",
        _ if var(vars, "LC_TERMINAL") == Some("iTerm2") => "Option",
        _ if ssh(vars) || multiplexer(vars).is_some() || macos => {
            "Shift (Option in iTerm2, Fn in Terminal.app)"
        }
        _ => "Shift",
    }
}

/// VS Code 1.123、1.124 的 OSC 52 会弄乱非 ASCII。
fn mojibake(text: &str, vars: &Vars) -> bool {
    if var(vars, "TERM_PROGRAM") != Some("vscode") || text.is_ascii() {
        return false;
    }
    let version: Vec<u64> = var(vars, "TERM_PROGRAM_VERSION")
        .unwrap_or_default()
        .split(['.', '-'])
        .take(3)
        .map_while(|part| part.parse().ok())
        .collect();
    matches!(version.as_slice(), [1, minor, _] if (123..125).contains(minor))
}

/// 复制后显示的提示：文字与显示时长。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub duration: Duration,
}

/// 复制了 `text`、途径是 `route` 时的提示。
pub fn notice(route: CopyRoute, text: &str, vars: &Vars, lang: Lang) -> Notice {
    let count = text.chars().count();
    let macos = cfg!(target_os = "macos");
    let body = match (route, lang) {
        (CopyRoute::Native, Lang::En) => {
            format!("copied {count} {} to clipboard", chars(count))
        }
        (CopyRoute::Native, Lang::Zh) => format!("已复制 {count} 个字符到剪贴板"),
        (CopyRoute::TmuxBuffer, Lang::En) => format!(
            "copied {count} {} to tmux buffer · paste with prefix + ]",
            chars(count)
        ),
        (CopyRoute::TmuxBuffer, Lang::Zh) => {
            format!("已复制 {count} 个字符到 tmux 缓冲区 · 用 prefix + ] 粘贴")
        }
        (CopyRoute::Osc52, Lang::En) => format!(
            "sent {count} {} via OSC 52 · if paste fails, hold {} while selecting for native copy",
            chars(count),
            native_key(vars, macos)
        ),
        (CopyRoute::Osc52, Lang::Zh) => format!(
            "已通过 OSC 52 发送 {count} 个字符 · 粘贴不出来时按住 {} 再拖选，用终端自己的复制",
            native_key(vars, macos)
        ),
    };
    let warn = route == CopyRoute::Osc52 && mojibake(text, vars);
    let text = if warn {
        let warning = match lang {
            Lang::En => "VS Code 1.123/1.124 will mojibake this paste — update to ≥1.125",
            Lang::Zh => "VS Code 1.123/1.124 粘贴出来会乱码，请升级到 1.125 及以上",
        };
        format!("⚠ {warning} · {body}")
    } else {
        body
    };
    let duration = match (warn, route) {
        (true, _) => Duration::from_secs(6),
        (false, CopyRoute::Native) => Duration::from_secs(2),
        (false, _) => Duration::from_secs(4),
    };
    Notice { text, duration }
}

fn chars(count: usize) -> &'static str {
    if count == 1 { "char" } else { "chars" }
}

#[cfg(test)]
#[path = "clipboard_tests.rs"]
mod tests;
