//! 按这一步的输入算的提醒（docs/specs/rust-cli/context.md §4.4）：`ultrathink_effort` 与 `skill_mention`。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::listing::{ListingSource, Listings};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::{ModelSelection, QuerySource};
use rust_cli_kernel_api::orchestration::{ContextBuilder, ContextRequest, OrchestrationError};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{FixedClock, SequentialIds, UnusedEnvironment};

use super::has_ultrathink;
use crate::{ContextConfig, ContextPorts, PromptAssembler, Surface};

const ULTRATHINK_TEXT: &str = "The user included the keyword \"ultrathink\", requesting deeper reasoning on this turn. Reason as thoroughly as the task warrants.";

/// 名字表：输入里出现的 `/greet`、`/helper`（按出现顺序）。
struct Skills;

#[async_trait]
impl ListingSource for Skills {
    async fn listings(&self, _request: &ContextRequest) -> Result<Listings, OrchestrationError> {
        Ok(Listings::default())
    }

    async fn skill_mentions(
        &self,
        _request: &ContextRequest,
        input: &str,
    ) -> Result<Vec<String>, OrchestrationError> {
        let mut found: Vec<(usize, &str)> = ["greet", "helper"]
            .into_iter()
            .filter_map(|name| input.find(&format!("/{name}")).map(|at| (at, name)))
            .collect();
        found.sort();
        Ok(found.into_iter().map(|(_, name)| name.to_owned()).collect())
    }
}

fn request(source: QuerySource, tools: &[&str]) -> ContextRequest {
    ContextRequest {
        default_machine: None,
        capabilities: None,
        session: SessionId::new("s"),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/w"),
        },
        agent: None,
        source,
        mode: PermissionMode::Default,
        tool_names: tools.iter().map(|name| (*name).to_owned()).collect(),
        selection: ModelSelection {
            provider: "p".to_owned(),
            model: rust_cli_base::models::GLM_5_3.to_owned(),
            reasoning: None,
            connection: None,
        },
        plan_flags: Default::default(),
        mcp: None,
    }
}

fn assembler(surface: Surface) -> PromptAssembler {
    let env = Arc::new(UnusedEnvironment);
    PromptAssembler::new(
        ContextPorts {
            info: env.clone(),
            files: env.clone(),
            shell: env.clone(),
            git: env,
            clock: Arc::new(FixedClock(1_792_224_000_000)),
            ids: Arc::new(SequentialIds::default()),
            listings: Arc::new(Skills),
        },
        ContextConfig::new(surface),
    )
}

fn user(id: &str, text: &str, origin: UserOrigin) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        origin,
    })
}

fn typed(text: &str) -> Vec<Message> {
    vec![user("u", text, UserOrigin::Human)]
}

async fn turn(
    surface: Surface,
    source: QuerySource,
    tools: &[&str],
    history: &[Message],
) -> Vec<(String, String)> {
    assembler(surface)
        .turn_attachments(request(source, tools), history)
        .await
        .unwrap()
        .into_iter()
        .map(|item| (item.source, item.text))
        .collect()
}

fn sources(items: &[(String, String)]) -> Vec<&str> {
    items.iter().map(|(source, _)| source.as_str()).collect()
}

#[tokio::test]
async fn ultrathink_follows_the_model_identity() {
    let items = turn(
        Surface::Print,
        QuerySource::MainThread,
        &[],
        &typed("Please UltraThink before the checks."),
    )
    .await;
    assert_eq!(
        sources(&items),
        ["model", "ultrathink_effort", "total_tokens_reminder"]
    );
    assert_eq!(
        items[1].1,
        format!("<system-reminder>\n{ULTRATHINK_TEXT}\n</system-reminder>")
    );
}

#[test]
fn ultrathink_is_a_whole_ascii_word_in_any_case() {
    // JS 的 `\b` 只认 ASCII 单词字符：`é` 不是单词字符，挨着它也算边界。
    for yes in [
        "ultrathink",
        "ULTRATHINK it",
        "(ultrathink)",
        "go-ultrathink.",
        "éultrathink",
    ] {
        assert!(has_ultrathink(yes), "{yes}");
    }
    for no in [
        "ultrathinking",
        "x_ultrathink",
        "ultrathink2",
        "ultra think",
        "ultrathınk",
    ] {
        assert!(!has_ultrathink(no), "{no}");
    }
}

#[tokio::test]
async fn ultrathink_is_only_for_the_input_step() {
    let mut history = typed("ultrathink please");
    history.push(Message::Assistant(AssistantMessage {
        id: MessageId::new("a"),
        content: vec![ContentBlock::ToolUse {
            id: ToolUseId::new("t"),
            name: "Read".to_owned(),
            input: serde_json::json!({}),
        }],
        model: "m".to_owned(),
        stop_reason: None,
        usage: None,
    }));
    history.push(Message::User(UserMessage {
        id: MessageId::new("r"),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new("t"),
            content: vec![ToolResultContent::Text {
                text: "ok".to_owned(),
            }],
            is_error: false,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new("a"),
        },
    }));
    let items = turn(Surface::Print, QuerySource::MainThread, &[], &history).await;
    assert!(!sources(&items).contains(&"ultrathink_effort"));
    // 子代理不发。
    let items = turn(
        Surface::Print,
        QuerySource::Subagent("general-purpose".to_owned()),
        &[],
        &typed("ultrathink please"),
    )
    .await;
    assert!(!sources(&items).contains(&"ultrathink_effort"));
}

#[tokio::test]
async fn a_user_invoked_skill_is_checked_by_its_body() {
    let history = vec![
        user(
            "u",
            "<command-message>greet</command-message>\n<command-name>/greet</command-name>\n<command-args>Ada</command-args>",
            UserOrigin::Human,
        ),
        user(
            "s",
            "Base directory for this skill: /w/.zcode/skills/greet\n\nUltrathink, then greet Ada.",
            UserOrigin::Meta {
                source: "skill".to_owned(),
            },
        ),
    ];
    let items = turn(Surface::Print, QuerySource::MainThread, &[], &history).await;
    assert!(sources(&items).contains(&"ultrathink_effort"));
}

#[tokio::test]
async fn typed_skill_names_come_first_in_the_terminal() {
    let input = typed("Please run /greet for Ada and mention /helper too.");
    let items = turn(
        Surface::Interactive,
        QuerySource::MainThread,
        &["Skill"],
        &input,
    )
    .await;
    assert_eq!(
        sources(&items)[..3],
        ["skill_mention", "skill_mention", "model"]
    );
    assert_eq!(
        items[0].1,
        "<system-reminder>\nThe user's message contains /greet, which is the name of a skill. If they are asking you to run it, call the Skill tool with skill: \"greet\", passing any arguments they gave as args. If they only mention it, do not run it.\n</system-reminder>"
    );
    assert!(items[1].1.contains("contains /helper, "));
}

#[tokio::test]
async fn skill_mentions_need_a_typed_input_and_the_skill_tool() {
    let input = typed("Please run /greet for Ada.");
    // `-p` 的输入不算人类键入；没有 Skill 工具；子代理；斜杠命令开的回合。
    let print = turn(Surface::Print, QuerySource::MainThread, &["Skill"], &input).await;
    let no_tool = turn(Surface::Interactive, QuerySource::MainThread, &[], &input).await;
    let subagent = turn(
        Surface::Interactive,
        QuerySource::Subagent("general-purpose".to_owned()),
        &["Skill"],
        &input,
    )
    .await;
    let command = typed(
        "<command-message>greet</command-message>\n<command-name>/greet</command-name>\n<command-args>/helper</command-args>",
    );
    let command = turn(
        Surface::Interactive,
        QuerySource::MainThread,
        &["Skill"],
        &command,
    )
    .await;
    for items in [print, no_tool, subagent, command] {
        assert!(!sources(&items).contains(&"skill_mention"), "{items:?}");
    }
}
