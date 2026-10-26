//! skill 正文的生成（docs/specs/rust-cli/ext.md §2.4 第 1–4 步）：Skill 工具与用户调用（§2.7）共用。
//!
//! 文件 skill：`Base directory for this skill: <目录>` 加正文，参数替换、变量、`!` 内联命令；内置 skill 是
//! 生成的提示（tools-more.md §17.1）。

use rust_cli_base::names::vars;
use rust_cli_kernel_api::tool::ToolUseContext;

use super::inline::Runner;
use super::{Skill, SkillCatalog, SkillSource, args, builtin};

/// 变量替换（ext.md §2.4 第 3 步）：两套名字都认。
fn variables(text: String, skill: &Skill, context: &ToolUseContext) -> String {
    let dir = if skill.dir.contains('\\') {
        skill.dir.replace('\\', "/")
    } else {
        skill.dir.clone()
    };
    let root = context.workspace.root.as_str();
    let session = context.session.as_str();
    [
        (vars::CLAUDE_SKILL_DIR, dir.as_str()),
        (vars::ZCODE_SKILL_DIR, dir.as_str()),
        (vars::CLAUDE_PROJECT_DIR, root),
        (vars::ZCODE_PROJECT_DIR, root),
        (vars::CLAUDE_SESSION_ID, session),
        (vars::ZCODE_SESSION_ID, session),
    ]
    .iter()
    .fold(text, |text, (name, value)| {
        text.replace(&vars::placeholder(name), value)
    })
}

/// skill 的正文；`!` 命令被拒绝或失败时返回错误文本（`Shell command … failed for pattern …`）。
pub(crate) async fn render(
    catalog: &SkillCatalog,
    runner: Option<&Runner>,
    skill: &Skill,
    args: &str,
    context: &ToolUseContext,
) -> Result<String, String> {
    if skill.source == SkillSource::Builtin {
        if let Some(body) = builtin::prompt_command(catalog, skill, context).await {
            return body;
        }
        // 在 git 仓库里的 `/security-review`：正文的 `!` 命令照 skill 的规则执行。
        if skill.name == builtin::SECURITY_REVIEW {
            let content = rust_cli_prompts::commands::security_review().to_owned();
            return match runner {
                Some(runner) => runner.expand(content, &skill.allowed_tools, context).await,
                None => Ok(content),
            };
        }
        return Ok(builtin::prompt(catalog, context, args).await);
    }
    let content = format!(
        "Base directory for this skill: {}\n\n{}",
        skill.dir, skill.body
    );
    let content = args::substitute(&content, args, &skill.arg_names);
    let content = variables(content, skill, context);
    match runner {
        Some(runner) => runner.expand(content, &skill.allowed_tools, context).await,
        None => Ok(content),
    }
}
