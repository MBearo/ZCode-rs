use std::sync::Arc;

use rust_cli_kernel_api::message::{ContentBlock, Message, ToolResultContent};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{MemoryEnvironment, SequentialIds};
use rust_cli_kernel_api::tool::Tool;
use serde_json::json;
use tokio::sync::mpsc;

use super::*;
use crate::skills::SkillTool;
use crate::test_support::{CWD, HOME, facts, tool_context, workspace};

#[test]
fn interval_only_inputs_are_recognized() {
    assert_eq!(interval_only("5m").as_deref(), Some("5m"));
    assert_eq!(interval_only("every 2 hours").as_deref(), Some("2h"));
    assert_eq!(interval_only("Every 20 MIN").as_deref(), Some("20m"));
    assert_eq!(interval_only("every 30 secs").as_deref(), Some("30s"));
    assert_eq!(interval_only("check the deploy every 20m"), None);
    assert_eq!(interval_only("5m /babysit-prs"), None);
}

async fn launch(env: MemoryEnvironment, args: &str) -> String {
    let env = Arc::new(env);
    let catalog = Arc::new(SkillCatalog::new(env.clone()));
    let tool = SkillTool::new(catalog, Arc::new(SequentialIds::default()), None);
    let (progress, _rx) = mpsc::channel(1);
    let context = tool_context(env, PermissionMode::Default);
    let input = json!({"skill": "loop", "args": args});
    assert_eq!(
        tool.validate_input(&input, &context).await,
        rust_cli_kernel_api::tool::Validation::Ok
    );
    let result = tool
        .call_with_messages(input, context, progress)
        .await
        .unwrap();
    assert_eq!(
        result.output.content,
        [ToolResultContent::Text {
            text: "Launching skill: loop".to_owned()
        }]
    );
    match &result.new_messages[..] {
        [Message::User(user)] => match &user.content[..] {
            [ContentBlock::Text { text }] => text.clone(),
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
}

/// 有 prompt：普通提示，原输入在最后；没有 `Base directory` 前缀。
#[tokio::test]
async fn a_prompt_gets_the_parsing_instructions() {
    let text = launch(MemoryEnvironment::new(facts()), "  check the deploy ").await;
    assert!(text.starts_with("# /loop \u{2014} schedule a recurring or self-paced prompt\n\n"));
    assert!(text.ends_with("## Input\n\ncheck the deploy"));
    assert!(!text.contains("Base directory"));
}

/// 没有参数：自主默认、动态节奏；只有间隔：固定间隔；有 loop.md 时用 loop.md（项目的优先于 home）。
#[tokio::test]
async fn empty_and_interval_only_inputs_use_the_defaults() {
    let dynamic = launch(MemoryEnvironment::new(facts()), "").await;
    assert!(dynamic.starts_with("# /loop \u{2014} autonomous default with dynamic pacing"));
    assert!(dynamic.ends_with(loops::preamble()));
    let fixed = launch(MemoryEnvironment::new(facts()), "every 5 minutes").await;
    assert!(fixed.contains("(input was empty or just the interval `5m`)"));
    assert!(fixed.contains("1. Convert `5m` to a 5-field cron expression."));
    let env = MemoryEnvironment::new(facts())
        .with_file(&format!("{HOME}/.zcode/loop.md"), "- home task\n")
        .with_file(&format!("{CWD}/.zcode/loop.md"), "\n- project task\n");
    let file = launch(env, "").await;
    let path = format!("{CWD}/.zcode/loop.md");
    assert!(file.contains(&format!("has a loop-tasks file at `{path}`")));
    assert!(file.ends_with(&format!("## Loop tasks (from {path})\n\n- project task")));
}

/// loop.md 的位置与规则文件、子代理、skill 一样用执行环境的 `PathStyle::join` 拼（修复线 F58）：home
/// 里有 `..` 时也找得到。以前按路径里有没有 `\` 选分隔符、只去末尾的分隔符，得到 `/Users/x/../dev/.zcode/loop.md`。
#[tokio::test]
async fn the_loop_file_path_is_joined_like_the_other_dot_dirs() {
    let mut odd = facts();
    odd.home = Some(rust_cli_kernel_api::env::EnvPath::new("/Users/x/../dev/"));
    let env =
        MemoryEnvironment::new(odd).with_file(&format!("{HOME}/.zcode/loop.md"), "- home task\n");
    let file = launch(env, "").await;
    let path = format!("{HOME}/.zcode/loop.md");
    assert!(
        file.ends_with(&format!("## Loop tasks (from {path})\n\n- home task")),
        "{file}"
    );
}

/// 清单里 `loop` 排在文件 skill 之后；同名的文件 skill 生效。
#[tokio::test]
async fn the_builtin_comes_after_file_skills() {
    let env = MemoryEnvironment::new(facts()).with_file(
        &format!("{CWD}/.zcode/skills/greet/SKILL.md"),
        "---\ndescription: Greet\n---\n",
    );
    let catalog = SkillCatalog::new(Arc::new(env));
    let lines: Vec<String> = catalog
        .listing(&workspace(), &[], "")
        .await
        .into_iter()
        .map(|entry| entry.line)
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], "- greet: Greet");
    assert_eq!(lines[1], format!("- loop: {DESCRIPTION} - {WHEN_TO_USE}"));
    let shadow = MemoryEnvironment::new(facts()).with_file(
        &format!("{CWD}/.zcode/skills/loop/SKILL.md"),
        "---\ndescription: Mine\n---\n",
    );
    let catalog = SkillCatalog::new(Arc::new(shadow));
    let skill = catalog.find(&workspace(), "loop").await.unwrap();
    assert_eq!(skill.description, "Mine");
}

/// 内置的 `loop` 不算“只有安全字段”：交给判决链询问（bundled skill 带不在安全名单里的字段）。
#[tokio::test]
async fn the_builtin_needs_permission() {
    let env = Arc::new(MemoryEnvironment::new(facts()));
    let catalog = Arc::new(SkillCatalog::new(env.clone()));
    let tool = SkillTool::new(catalog, Arc::new(SequentialIds::default()), None);
    let context = tool_context(env, PermissionMode::Default);
    assert_eq!(
        tool.check_permissions(&json!({"skill": "loop"}), &context)
            .await,
        rust_cli_kernel_api::permission::PermissionDecision::Passthrough
    );
}

/// Monitor 的超时随形态：单次的 `-p` 是 10 分钟，TUI 30 分钟。
#[tokio::test]
async fn the_monitor_timeout_follows_the_surface() {
    let tui = launch(MemoryEnvironment::new(facts()), "check").await;
    assert!(tui.contains("arm one now with `timeout_ms: 1800000`"));
    assert!(tui.contains("A monitor expires after at most 30 minutes"));
    let env = Arc::new(MemoryEnvironment::new(facts()));
    let catalog = Arc::new(SkillCatalog::new(env.clone()));
    catalog.set_print_session(true);
    let tool = SkillTool::new(catalog, Arc::new(SequentialIds::default()), None);
    let (progress, _rx) = mpsc::channel(1);
    let context = tool_context(env, PermissionMode::Default);
    let result = tool
        .call_with_messages(json!({"skill": "loop", "args": ""}), context, progress)
        .await
        .unwrap();
    let Message::User(user) = &result.new_messages[0] else {
        panic!("a user message");
    };
    let [ContentBlock::Text { text }] = &user.content[..] else {
        panic!("one text block");
    };
    assert!(text.contains("arm one now with `timeout_ms: 600000`"));
    assert!(text.contains("A monitor expires after at most 10 minutes"));
}
