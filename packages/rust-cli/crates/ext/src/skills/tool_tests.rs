//! Skill 工具（ext.md §2.4）：结果形状与错误文本。

use std::sync::Arc;

use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{MemoryEnvironment, SequentialIds};
use rust_cli_policy::PolicyGate;
use serde_json::json;

use super::*;
use crate::skills::runner;
use crate::test_support::{CWD, EchoBash, facts, tool_context};

const GREET: &str = "---\ndescription: Greet someone by name\nwhen_to_use: When the user asks for a greeting\nargument-hint: <name>\narguments: [who]\nallowed-tools: Bash(echo:*)\n---\nSay hello to $ARGUMENTS ($who, first: $0).\nSkill dir: ${CLAUDE_SKILL_DIR}\nInline: !`echo inline-ok`\n";
const HELPER: &str = "---\nname: Helper\ndescription: A helper the model must not call\ndisable-model-invocation: true\n---\nHidden helper.\n";

fn env() -> Arc<MemoryEnvironment> {
    Arc::new(
        MemoryEnvironment::new(facts())
            .with_file(&format!("{CWD}/.zcode/skills/greet/SKILL.md"), GREET)
            .with_file(&format!("{CWD}/.zcode/skills/helper/SKILL.md"), HELPER)
            .with_file(
                &format!("{CWD}/.zcode/skills/plain/SKILL.md"),
                "# Plain skill\n\nNo frontmatter here.\n",
            )
            .with_file(
                &format!("{CWD}/.zcode/skills/vars/SKILL.md"),
                "${ZCODE_PROJECT_DIR} ${CLAUDE_SESSION_ID}\n",
            ),
    )
}

fn tool(env: Arc<MemoryEnvironment>) -> SkillTool {
    let catalog = Arc::new(SkillCatalog::new(env.clone(), env));
    let ids: Arc<dyn Ids> = Arc::new(SequentialIds::default());
    let bash = Arc::new(EchoBash::default());
    SkillTool::new(
        catalog,
        ids.clone(),
        Some(runner(bash, Arc::new(PolicyGate::default()), ids)),
    )
}

fn text(output: &ToolOutput) -> &str {
    match &output.content[..] {
        [ToolResultContent::Text { text }] => text,
        other => panic!("unexpected content {other:?}"),
    }
}

fn body(call: &ToolCallOutput) -> &str {
    match &call.new_messages[..] {
        [
            Message::User(UserMessage {
                content,
                origin: UserOrigin::Meta { source },
                ..
            }),
        ] if source == SKILL_MESSAGE_SOURCE => match &content[..] {
            [ContentBlock::Text { text }] => text,
            other => panic!("unexpected content {other:?}"),
        },
        other => panic!("unexpected messages {other:?}"),
    }
}

#[tokio::test]
async fn k1_greet_loads_the_body_after_the_result() {
    let env = env();
    let tool = tool(env.clone());
    let context = tool_context(env, PermissionMode::DontAsk);
    let (progress, _ignored) = mpsc::channel(4);
    let call = tool
        .call_with_messages(json!({"skill": "greet", "args": "Ada"}), context, progress)
        .await
        .unwrap();
    assert_eq!(text(&call.output), "Launching skill: greet");
    assert_eq!(
        body(&call),
        format!(
            "Base directory for this skill: {CWD}/.zcode/skills/greet\n\nSay hello to Ada (Ada, first: Ada).\nSkill dir: {CWD}/.zcode/skills/greet\nInline: out:echo inline-ok\n"
        )
    );
}

#[tokio::test]
async fn k1_plain_and_variables() {
    let env = env();
    let tool = tool(env.clone());
    let (progress, _ignored) = mpsc::channel(4);
    let context = tool_context(env.clone(), PermissionMode::DontAsk);
    let call = tool
        .call_with_messages(
            json!({"skill": "/plain", "args": "extra words"}),
            context.clone(),
            progress.clone(),
        )
        .await
        .unwrap();
    assert_eq!(text(&call.output), "Launching skill: plain");
    assert_eq!(
        body(&call),
        format!(
            "Base directory for this skill: {CWD}/.zcode/skills/plain\n\n# Plain skill\n\nNo frontmatter here.\n\n\nARGUMENTS: extra words"
        )
    );
    let vars = tool
        .call_with_messages(json!({"skill": "vars"}), context, progress)
        .await
        .unwrap();
    assert!(
        body(&vars).ends_with(&format!("{CWD} sess_1\n")),
        "{}",
        body(&vars)
    );
}

#[tokio::test]
async fn k1_validation_errors() {
    let env = env();
    let tool = tool(env.clone());
    let context = tool_context(env, PermissionMode::DontAsk);
    let message = |validation: Validation| match validation {
        Validation::Invalid { message, .. } => message,
        Validation::Ok => panic!("expected an error"),
    };
    assert_eq!(
        message(
            tool.validate_input(&json!({"skill": "nope"}), &context)
                .await
        ),
        "Unknown skill: nope"
    );
    assert_eq!(
        message(
            tool.validate_input(&json!({"skill": "gret"}), &context)
                .await
        ),
        "Unknown skill: gret. Did you mean greet?"
    );
    assert_eq!(
        message(tool.validate_input(&json!({"skill": "  "}), &context).await),
        "Invalid skill format:   "
    );
    assert_eq!(
        message(
            tool.validate_input(&json!({"skill": "helper"}), &context)
                .await
        ),
        "Skill helper cannot be used with Skill tool due to disable-model-invocation. Ask the user to run /helper themselves — it cannot be invoked via the Skill tool. Do not replicate this skill's workflow by other means — it is reserved for explicit user invocation."
    );
    assert_eq!(
        tool.validate_input(&json!({"skill": "greet"}), &context)
            .await,
        Validation::Ok
    );
}

#[tokio::test]
async fn only_safe_skills_skip_the_permission_chain() {
    let env = env();
    let tool = tool(env.clone());
    let context = tool_context(env, PermissionMode::Default);
    let decision = |name: &'static str| {
        let tool = &tool;
        let context = &context;
        async move {
            tool.check_permissions(&json!({"skill": name}), context)
                .await
        }
    };
    assert_eq!(
        decision("plain").await,
        PermissionDecision::Allow {
            updated_input: None
        }
    );
    assert_eq!(decision("greet").await, PermissionDecision::Passthrough);
}

#[tokio::test]
async fn the_k1_dont_ask_run_asks_for_greet_and_admits_plain() {
    // K1 第一次抓取（没有 allow 规则）：greet 带 allowed-tools，dontAsk 下被拒；plain 放行。
    let env = env();
    let tool = tool(env.clone());
    let context = tool_context(env, PermissionMode::DontAsk);
    let gate = PolicyGate::default();
    use rust_cli_kernel_api::orchestration::PermissionGate;
    let greet = gate
        .decide(&tool, &json!({"skill": "greet", "args": "Ada"}), &context)
        .await;
    let PermissionDecision::Deny { message } = greet else {
        panic!("expected a denial, got {greet:?}");
    };
    assert!(
        message.starts_with(
            "Permission to use Skill has been denied because ZCode is running in don't ask mode."
        ),
        "{message}"
    );
    let plain = gate
        .decide(&tool, &json!({"skill": "plain"}), &context)
        .await;
    assert!(
        matches!(plain, PermissionDecision::Allow { .. }),
        "{plain:?}"
    );
}
