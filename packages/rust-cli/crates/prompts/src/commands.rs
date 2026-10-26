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

/// `/rename <名字>` 记进对话时给模型的提醒（修复线 L-A，docs/specs/rust-cli/tui-manage.md §11.2；包进
/// `<system-reminder>`）。
pub fn rename_named(name: &str) -> String {
    let raw = include_lf!("../../../prompts/commands/rename_named.txt");
    crate::reminder::wrap(&crate::texts::render(crate::trim(raw), &[("name", name)]))
}

/// 内置提示词命令 `/init` 的正文（修复线 L-A，docs/specs/rust-cli/ext.md §2.9；导入其他工具配置的那一条
/// 不带（对照实现里它缺省关），文件头去掉对照实现的网址）。
pub fn init() -> &'static str {
    crate::trim(include_lf!("../../../prompts/commands/init.txt"))
}

/// 内置提示词命令 `/security-review` 的正文（对照实现的正文去掉 frontmatter；`!` 命令由 skill 的展开执行）。
pub fn security_review() -> &'static str {
    crate::trim(include_lf!("../../../prompts/commands/security_review.txt"))
}

/// 工作目录不是 git 仓库时 `/security-review` 的正文（对照实现同一处的另一个分支）。
pub fn security_review_not_git(cwd: &str) -> String {
    let raw = include_lf!("../../../prompts/commands/security_review_not_git.txt");
    crate::texts::render(crate::trim(raw), &[("cwd", cwd)])
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_prompt_commands_use_the_product_names() {
        assert!(
            super::init().starts_with("Please analyze this codebase and create a AGENTS.md file")
        );
        assert!(super::init().ends_with(
            "This file provides guidance to ZCode when working with code in this repository.\n```"
        ));
        assert!(super::security_review().contains("!`git diff origin/HEAD...`"));
        assert!(super::security_review_not_git("/w").contains("(`/w`) is not one."));
    }

    #[test]
    fn the_rename_reminder_quotes_the_name_inside_a_reminder() {
        assert_eq!(
            super::rename_named("my-name"),
            "<system-reminder>\nThe user named this session \"my-name\". This may indicate the session's focus or intent.\n</system-reminder>"
        );
    }

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
