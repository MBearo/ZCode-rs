use std::sync::Arc;

use rust_cli_kernel_api::ids::OperationId;
use rust_cli_kernel_api::permission::{PermissionDecision, PermissionMode};
use rust_cli_kernel_api::testing::{FixedClock, ScriptedInteractions};
use rust_cli_kernel_api::tool::{PromptContext, Tool, ToolUseContext, Validation};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use rust_cli_kernel_api::ids::SessionId;

use super::*;
use crate::test_more::{Ctx, output_text};

/// 2026-10-17T10:07:30Z。
const NOW: u64 = 1_792_231_650_000;

fn board() -> CronBoard {
    CronBoard::new(Arc::new(FixedClock(NOW)), None)
}

fn context(operation: &str, mode: PermissionMode) -> ToolUseContext {
    let mut ctx = Ctx::new(Arc::new(ScriptedInteractions::default()));
    ctx.mode = mode;
    let mut context = ctx.build();
    context.operation.operation = OperationId::new(operation);
    context
}

fn tool(board: &CronBoard, name: &str) -> Arc<dyn Tool> {
    cron_tools(board)
        .into_iter()
        .find(|tool| tool.name() == name)
        .unwrap_or_else(|| panic!("{name}"))
}

async fn call(tool: &Arc<dyn Tool>, input: Value, context: ToolUseContext) -> String {
    let (progress, _rx) = mpsc::channel(1);
    let output = tool
        .call(input, context, progress)
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    output_text(&output)
}

fn error(validation: Validation) -> (String, Option<i32>) {
    match validation {
        Validation::Invalid {
            message,
            error_code,
        } => (message, error_code),
        Validation::Ok => panic!("expected an error"),
    }
}

#[test]
fn definitions_are_deferred_with_search_hints() {
    let board = board();
    let prompt = PromptContext {
        tool_names: Vec::new(),
        mode: PermissionMode::Default,
    };
    for (tool, hint) in cron_tools(&board).iter().zip([
        "schedule a recurring or one-shot prompt",
        "cancel a scheduled cron job",
        "list active cron jobs",
    ]) {
        assert!(tool.should_defer());
        assert_eq!(tool.search_hint(), Some(hint));
        assert!(!tool.description(&prompt).is_empty(), "{}", tool.name());
        assert_eq!(tool.input_schema()["additionalProperties"], json!(false));
    }
    let create = tool(&board, CREATE).description(&prompt);
    assert!(create.starts_with("Schedule a prompt to be enqueued at a future time."));
    // 持久任务打开时的文本（tools-more.md §16.6，D17 取消）。
    assert!(create.contains("## Durability"));
    assert!(create.contains("Pass durable: true to write to .zcode/scheduled_tasks.json"));
    let schema = tool(&board, CREATE).input_schema();
    assert!(
        schema["properties"]["durable"]["description"]
            .as_str()
            .unwrap()
            .starts_with("true = persist to .zcode/scheduled_tasks.json")
    );
}

#[tokio::test]
async fn create_validates_its_input() {
    let board = board();
    let create = tool(&board, CREATE);
    let ctx = context("op_1", PermissionMode::Default);
    let check = |cron: &str| json!({"cron": cron, "prompt": "p"});
    assert_eq!(
        error(create.validate_input(&check("* * * *"), &ctx).await),
        (
            "Invalid cron expression '* * * *'. Expected 5 fields: M H DoM Mon DoW.".to_owned(),
            Some(1)
        )
    );
    assert_eq!(
        error(create.validate_input(&check("30 2 31 2 *"), &ctx).await),
        (
            "Cron expression '30 2 31 2 *' does not match any calendar date in the next year."
                .to_owned(),
            Some(2)
        )
    );
    for index in 0..MAX_JOBS {
        board.add(
            &ctx.session,
            format!("{index:08x}"),
            "*/5 * * * *",
            "p",
            true,
        );
    }
    assert_eq!(
        error(create.validate_input(&check("*/5 * * * *"), &ctx).await),
        (
            "Too many scheduled jobs (max 50). Cancel one first.".to_owned(),
            Some(3)
        )
    );
}

#[tokio::test]
async fn create_reports_the_job_and_lists_it() {
    let board = board();
    let create = tool(&board, CREATE);
    let op = "op_1a2b3c4d-0000-4000-8000-000000000000";
    let text = call(
        &create,
        json!({"cron": "*/5 * * * *", "prompt": "check the deploy"}),
        context(op, PermissionMode::Default),
    )
    .await;
    assert_eq!(
        text,
        "Scheduled recurring job 1a2b3c4d (Every 5 minutes). Session-only (not written to disk, dies when ZCode exits). Auto-expires after 7 days. Use CronDelete to cancel sooner."
    );
    let text = call(
        &create,
        json!({"cron": "0 9 * * 1-5", "prompt": "line one\nline two", "recurring": false}),
        context("op_ffffffff-0000", PermissionMode::Default),
    )
    .await;
    assert_eq!(
        text,
        "Scheduled one-shot task ffffffff (Weekdays at 9:00 AM). Session-only (not written to disk, dies when ZCode exits). It will fire once then auto-delete."
    );
    let list = call(
        &tool(&board, LIST),
        json!({}),
        context("op_2", PermissionMode::Default),
    )
    .await;
    assert_eq!(
        list,
        "1a2b3c4d \u{2014} Every 5 minutes (recurring) [session-only]: check the deploy\nffffffff \u{2014} Weekdays at 9:00 AM (one-shot) [session-only]: line one\u{2026}"
    );
}

#[tokio::test]
async fn delete_and_empty_list() {
    let board = board();
    let delete = tool(&board, DELETE);
    let ctx = context("op_1", PermissionMode::Default);
    assert_eq!(
        error(delete.validate_input(&json!({"id": "nope"}), &ctx).await),
        ("No scheduled job with id 'nope'".to_owned(), Some(1))
    );
    board.add(&ctx.session, "abcd0123".to_owned(), "7 * * * *", "p", true);
    assert_eq!(
        delete
            .validate_input(&json!({"id": "abcd0123"}), &ctx)
            .await,
        Validation::Ok
    );
    let text = call(&delete, json!({"id": "abcd0123"}), ctx.clone()).await;
    assert_eq!(text, "Cancelled job abcd0123.");
    let list = call(&tool(&board, LIST), json!({}), ctx).await;
    assert_eq!(list, "No scheduled jobs.");
}

#[tokio::test]
async fn auto_mode_goes_to_the_classifier() {
    let board = board();
    let create = tool(&board, CREATE);
    let input = json!({"cron": "7 * * * *", "prompt": "p"});
    assert_eq!(
        create
            .check_permissions(&input, &context("op_1", PermissionMode::Auto))
            .await,
        PermissionDecision::Passthrough
    );
    assert_eq!(
        create
            .check_permissions(&input, &context("op_1", PermissionMode::Default))
            .await,
        PermissionDecision::Allow {
            updated_input: None
        }
    );
}

#[test]
fn prompts_are_truncated_by_display_width() {
    assert_eq!(crate::cron_list::truncate_display("short", 80), "short");
    let long = "x".repeat(81);
    assert_eq!(
        crate::cron_list::truncate_display(&long, 80),
        format!("{}\u{2026}", "x".repeat(79))
    );
    // 全角字符占两列。
    let wide = "\u{4e2d}".repeat(41);
    assert_eq!(
        crate::cron_list::truncate_display(&wide, 80),
        format!("{}\u{2026}", "\u{4e2d}".repeat(39))
    );
    let first = format!("{}\nrest", "y".repeat(80));
    assert_eq!(
        crate::cron_list::truncate_display(&first, 80),
        format!("{}\u{2026}", "y".repeat(79))
    );
}

/// `durable: true`：写进文件（结果说明换成 Persisted），列出时排在会话任务前、不带 [session-only]，
/// 删除时从文件删（tools-more.md §16.6）。
#[tokio::test]
async fn durable_jobs_go_to_the_file() {
    let store = Arc::new(rust_cli_kernel_api::testing::MemoryCronStore::default());
    let board = CronBoard::new(Arc::new(FixedClock(NOW)), Some(store.clone()));
    let create = tool(&board, CREATE);
    let ctx = || {
        context(
            "op_1a2b3c4d-0000-4000-8000-000000000000",
            PermissionMode::Default,
        )
    };
    let text = call(
        &create,
        json!({"cron": "*/5 * * * *", "prompt": "check the deploy", "durable": true}),
        ctx(),
    )
    .await;
    assert_eq!(
        text,
        "Scheduled recurring job 1a2b3c4d (Every 5 minutes). Persisted to .zcode/scheduled_tasks.json. Auto-expires after 7 days. Use CronDelete to cancel sooner."
    );
    let stored = store.snapshot();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].created_at, NOW);
    let session = context(
        "op_ffffffff-0000-4000-8000-000000000000",
        PermissionMode::Default,
    );
    call(
        &create,
        json!({"cron": "0 9 * * 1-5", "prompt": "standup", "recurring": false}),
        session,
    )
    .await;
    let list = call(&tool(&board, LIST), json!({}), ctx()).await;
    assert_eq!(
        list,
        "1a2b3c4d \u{2014} Every 5 minutes (recurring): check the deploy\nffffffff \u{2014} Weekdays at 9:00 AM (one-shot) [session-only]: standup"
    );
    let delete = tool(&board, DELETE);
    assert_eq!(
        delete
            .validate_input(&json!({"id": "1a2b3c4d"}), &ctx())
            .await,
        Validation::Ok
    );
    call(&delete, json!({"id": "1a2b3c4d"}), ctx()).await;
    assert!(store.snapshot().is_empty());
    // 文件里的任务也计入 50 个的上限。
    assert_eq!(
        board.read_durable().await.len() + board.session_count(&SessionId::new("x")),
        0
    );
}
