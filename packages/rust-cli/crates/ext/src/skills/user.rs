//! 用户调用 skill：`/<名字> [参数]` 的展开（docs/specs/rust-cli/ext.md §2.7）：解析输入、找 skill、
//! 生成 prompt 命令的消息。
//!
//! 只给出文字与种类；消息 ID、提交与 UserPromptSubmit 由会话 actor 的回合任务决定。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::tool::ToolUseContext;
use rust_cli_kernel_api::user_command::{
    ExpandedKind, ExpandedMessage, Expansion, LOCAL_COMMAND_CAVEAT_SOURCE, LOCAL_COMMAND_SOURCE,
    UserCommand, UserCommands, command_input, command_tags,
};

use super::inline::Runner;
use super::{SKILL_MESSAGE_SOURCE, Skill, SkillCatalog};

/// 本地命令之前的 caveat（产品名替换，kernel-api 的定义，F55：以前这里另写了一份）。
const CAVEAT: &str = rust_cli_kernel_api::user_input::BASH_CAVEAT;

/// 用户可调用的 skill 命令。
pub struct SkillCommands {
    catalog: Arc<SkillCatalog>,
    /// `!` 命令的执行者（与 Skill 工具同一个）。
    runner: Option<Runner>,
}

impl SkillCommands {
    pub(crate) fn new(catalog: Arc<SkillCatalog>, runner: Option<Runner>) -> Self {
        Self { catalog, runner }
    }
}

/// 解析斜杠输入：trim 后以 `/` 开头；名字到第一个空白为止，其余 trim 后是参数。
pub(crate) fn parse(input: &str) -> Option<(&str, &str)> {
    let rest = input.trim().strip_prefix('/')?;
    let (name, args) = match rest.find(char::is_whitespace) {
        Some(at) => (&rest[..at], rest[at..].trim()),
        None => (rest, ""),
    };
    (!name.is_empty()).then_some((name, args))
}

/// 按名字精确找；没有时试 `<名字>:<第一个参数>`（`/<插件> <skill>` 写法），参数去掉这一段。
fn find<'a>(skills: &[Arc<Skill>], name: &str, args: &'a str) -> Option<(Arc<Skill>, &'a str)> {
    if let Some(skill) = skills.iter().find(|skill| skill.name == name) {
        return Some((skill.clone(), args));
    }
    let (first, rest) = match args.find(char::is_whitespace) {
        Some(at) => (&args[..at], args[at..].trim_start()),
        None => (args, ""),
    };
    if first.is_empty() {
        return None;
    }
    let qualified = format!("{name}:{first}");
    skills
        .iter()
        .find(|skill| skill.name == qualified)
        .map(|skill| (skill.clone(), rest))
}

fn input(text: String) -> ExpandedMessage {
    ExpandedMessage {
        text,
        kind: ExpandedKind::Input,
    }
}

fn meta(source: &str, text: String) -> ExpandedMessage {
    ExpandedMessage {
        text,
        kind: ExpandedKind::Meta {
            source: source.to_owned(),
        },
    }
}

/// HTML 文本转义。
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 本地结果（不调模型）：caveat、命令输入、结果（前面加 caveat）。
fn local(command: String, output: String) -> Expansion {
    Expansion::Messages {
        messages: vec![
            meta(LOCAL_COMMAND_CAVEAT_SOURCE, CAVEAT.to_owned()),
            input(command),
            meta(LOCAL_COMMAND_SOURCE, output),
        ],
        query: false,
    }
}

#[async_trait]
impl UserCommands for SkillCommands {
    async fn commands(&self, workspace: &WorkspaceRef) -> Vec<UserCommand> {
        self.catalog
            .skills(workspace)
            .await
            .iter()
            .map(|skill| UserCommand {
                name: skill.name.clone(),
                description: skill.description.clone(),
                argument_hint: skill.argument_hint.clone(),
                // `user-invocable: false` 不进 `/` 菜单，但输入时仍按命令处理（给出提示）。
                hidden: !skill.user_invocable,
            })
            .collect()
    }

    async fn expand(&self, text: &str, context: ToolUseContext) -> Expansion {
        let Some((name, args)) = parse(text) else {
            return Expansion::NotCommand;
        };
        let skills = self.catalog.skills(&context.workspace).await;
        let Some((skill, args)) = find(&skills, name, args) else {
            // `-p` 的回退：输入照常发给模型，后面跟一条提醒（TUI 在发送前自己提示未知命令）。
            if self.catalog.print_session()
                && super::unknown::applies(self.catalog.env.as_ref(), &context.workspace, name)
                    .await
            {
                return Expansion::Messages {
                    messages: vec![
                        input(text.to_owned()),
                        ExpandedMessage {
                            text: rust_cli_prompts::reminder::wrap(&super::unknown::text(
                                name, &skills,
                            )),
                            kind: ExpandedKind::Attachment {
                                source: super::unknown::SOURCE.to_owned(),
                            },
                        },
                    ],
                    query: true,
                };
            }
            return Expansion::NotCommand;
        };
        // `userInvocable === false` 时只回一句提示，不调模型（产品名替换，ext.md §2.7）。
        if !skill.user_invocable {
            return local(
                format!("/{}", skill.name),
                format!(
                    "This skill can only be invoked by ZCode, not directly by users. Ask ZCode to use the \"{}\" skill for you.",
                    skill.name
                ),
            );
        }
        match super::body::render(&self.catalog, self.runner.as_ref(), &skill, args, &context).await
        {
            Ok(body) => Expansion::Messages {
                messages: vec![
                    input(command_tags(&skill.name, args)),
                    meta(SKILL_MESSAGE_SOURCE, body),
                ],
                query: true,
            },
            // `!` 命令被拒绝或失败：写命令输入与 stderr，不调模型。
            Err(message) => local(
                command_input(&skill.name, args),
                format!(
                    "<local-command-stderr>{}</local-command-stderr>",
                    escape(&message)
                ),
            ),
        }
    }
}

#[cfg(test)]
#[path = "user_tests.rs"]
mod tests;
