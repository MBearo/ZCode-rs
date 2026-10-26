//! 快捷键表（docs/specs/rust-cli/tui-input.md §5）：`/help` 的“快捷键”
//! 一节用它。
use super::i18n::Lang;

/// 粘贴图片的键：Windows 是 alt+v，其他平台是 ctrl+v。
pub fn image_paste_key() -> &'static str {
    if cfg!(windows) { "alt + v" } else { "ctrl + v" }
}

/// 同上，提示里的写法（`ctrl+v`，没有空格）。
pub fn image_paste_chord() -> &'static str {
    if cfg!(windows) { "alt+v" } else { "ctrl+v" }
}

/// （按键，英文说明，中文说明）。按键写成提示里的紧凑形式（`ctrl+x enter`），不比最长的命令名宽，
/// `/help` 的命令列因此不变宽。
pub fn shortcuts() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "enter",
            "Send; while running, add to the current turn",
            "发送；运行中插进当前回合",
        ),
        (
            "ctrl+enter",
            "Send now (interrupts the running turn)",
            "立即发送（中断运行中的回合）",
        ),
        (
            "ctrl+x enter",
            "Queue until the turn ends",
            "排到回合结束后",
        ),
        (
            "shift+enter",
            "Newline (also alt+enter, ctrl+j)",
            "换行（alt+enter、ctrl+j 也可以）",
        ),
        ("ctrl+l", "Clear input", "清空输入"),
        (
            "ctrl+s",
            "Stash prompt (auto-restores after submit)",
            "暂存输入（发送后自动放回）",
        ),
        (
            "ctrl+g",
            "Edit in $EDITOR (also ctrl+x ctrl+e)",
            "用 $EDITOR 编辑（ctrl+x ctrl+e 也可以）",
        ),
        (image_paste_chord(), "Paste images", "粘贴图片"),
        (
            "ctrl+x ctrl+b",
            "Move running commands to the background",
            "把在跑的命令转到后台",
        ),
        ("meta+p", "Switch model", "切换模型"),
        ("!", "Shell mode", "shell 模式"),
        ("@", "File paths", "文件路径"),
        ("shift+tab", "Cycle permission modes", "切换权限模式"),
        ("ctrl+o", "Details", "查看详细"),
        ("ctrl+r", "Search history", "搜索历史"),
        (
            "esc",
            "Interrupt; double tap to clear input",
            "中断；连按两次清空输入",
        ),
        ("ctrl+_", "Undo", "撤销"),
    ]
}

/// 一条说明的当前语言。
pub fn describe(lang: Lang, entry: &(&'static str, &'static str, &'static str)) -> &'static str {
    match lang {
        Lang::En => entry.1,
        Lang::Zh => entry.2,
    }
}
