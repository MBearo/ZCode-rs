//! 本地斜杠命令的固定文字（docs/specs/rust-cli/tui-display.md §3）。L28 的 `/agents` 说明已随命令删除
//!（用户决定，tui-display.md §3.2，L33）。

use rust_cli_base::include_lf;

/// `/cd` 给模型的说明（docs/specs/rust-cli/tui-manage.md §7.1，包进
/// `<system-reminder>`）；`dir` 是新目录。不提项目设置、MCP、skill 与环境变量。
pub fn cd_moved(dir: &str) -> String {
    let raw = include_lf!("../../../prompts/commands/cd_moved.txt");
    let text = crate::texts::render(crate::trim(raw), &[("dir", dir)]);
    crate::reminder::wrap(&text)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_cd_note_names_the_directory_twice_inside_a_reminder() {
        let text = super::cd_moved("/Users/dev/other");
        assert!(text.starts_with("<system-reminder>\nThe session's working directory has changed to /Users/dev/other (via /cd)."));
        assert!(
            text.contains(
                "now resolve from /Users/dev/other; its AGENTS.md, if any, follows below."
            )
        );
        assert!(text.ends_with("</system-reminder>"));
    }
}
