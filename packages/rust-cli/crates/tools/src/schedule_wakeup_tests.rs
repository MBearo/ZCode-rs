use std::sync::Arc;

use rust_cli_kernel_api::ids::OperationId;
use rust_cli_kernel_api::permission::{PermissionDecision, PermissionMode};
use rust_cli_kernel_api::testing::{FixedClock, ScriptedInteractions};
use rust_cli_kernel_api::tool::{PromptContext, Tool, ToolError, ToolUseContext};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::*;
use crate::test_more::{Ctx, output_text};

/// 2026-10-17T10:07:30Z。
const NOW: u64 = 1_792_231_650_000;

fn context(mode: PermissionMode) -> ToolUseContext {
    let mut ctx = Ctx::new(Arc::new(ScriptedInteractions::default()));
    ctx.mode = mode;
    let mut context = ctx.build();
    context.operation.operation = OperationId::new("op_1a2b3c4d-0000-4000-8000-000000000000");
    context
}

async fn call(tool: &ScheduleWakeupTool, input: Value) -> Result<String, ToolError> {
    let (progress, _rx) = mpsc::channel(1);
    let output = tool
        .call(input, context(PermissionMode::Default), progress)
        .await?;
    Ok(output_text(&output))
}

/// 定义：不延迟，searchHint 与 maxResultSizeChars 固定。
#[test]
fn the_definition_comes_from_s0() {
    let tool = ScheduleWakeupTool::new(&CronBoard::new(Arc::new(FixedClock(NOW)), None));
    let prompt = PromptContext {
        tool_names: Vec::new(),
        mode: PermissionMode::Default,
    };
    assert_eq!(tool.name(), "ScheduleWakeup");
    assert!(!tool.should_defer());
    assert!(tool.description(&prompt).starts_with(
        "Schedule when to resume work in /loop dynamic mode \u{2014} the user invoked /loop without an interval"
    ));
    let properties = tool.input_schema()["properties"].clone();
    for key in ["delaySeconds", "reason", "prompt", "stop", "noop"] {
        assert!(properties.get(key).is_some(), "{key}");
    }
    assert_eq!(tool.max_result_size_chars(), Some(1_000));
    // 修复线 F51（tools-more.md §17.2）：描述只说这个会话的缓存期限。
    let description = tool.description(&prompt);
    assert!(
        description.contains("This session's requests use the default 5-minute prompt-cache TTL.")
    );
    assert!(
        tool.search_hint()
            .unwrap()
            .starts_with("self-pace the dynamic /loop")
    );
}

#[tokio::test]
async fn auto_mode_goes_to_the_classifier() {
    let tool = ScheduleWakeupTool::new(&CronBoard::new(Arc::new(FixedClock(NOW)), None));
    let input = json!({"delaySeconds": 60});
    assert_eq!(
        tool.check_permissions(&input, &context(PermissionMode::Auto))
            .await,
        PermissionDecision::Passthrough
    );
    assert!(matches!(
        tool.check_permissions(&input, &context(PermissionMode::Default))
            .await,
        PermissionDecision::Allow { .. }
    ));
}

/// 排上（整分钟、相对时间）、夹过的说明、停止（删掉 1 个、再停没有）。
#[tokio::test]
async fn schedule_and_stop_results_are_verbatim() {
    let board = CronBoard::new(Arc::new(FixedClock(NOW)), None);
    let tool = ScheduleWakeupTool::new(&board);
    let input = json!({"delaySeconds": 1200, "reason": "watching CI", "prompt": "/loop check", "noop": true});
    assert_eq!(
        call(&tool, input).await.unwrap(),
        "Next wakeup scheduled for 10:28:00 (in 1230s). If you owe the user a status update this tick, write it now as ordinary response text; then end the turn \u{2014} the harness re-invokes you when the wakeup fires or a task-notification arrives."
    );
    let clamped =
        json!({"delaySeconds": "30", "reason": "r", "prompt": "/loop check", "noop": false});
    assert!(call(&tool, clamped).await.unwrap().starts_with(
        "Next wakeup scheduled for 10:09:00 (in 90s) (clamped to 60s from your requested value)."
    ));
    let stopped = call(&tool, json!({"stop": true})).await.unwrap();
    assert!(stopped.starts_with("Loop stopped \u{2014} cancelled 1 pending wakeup(s);"));
    let again = call(&tool, json!({"stop": true})).await.unwrap();
    assert!(again.starts_with("Loop stopped \u{2014} any dynamic loop in this session is ended;"));
}

#[tokio::test]
async fn missing_fields_are_errors() {
    let tool = ScheduleWakeupTool::new(&CronBoard::new(Arc::new(FixedClock(NOW)), None));
    let message = |result: Result<String, ToolError>| match result {
        Err(ToolError::Failed { message }) => message,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        message(call(&tool, json!({"prompt": "p", "noop": true})).await),
        "`delaySeconds` and `reason` are required when `stop` is not true."
    );
    assert_eq!(
        message(
            call(
                &tool,
                json!({"delaySeconds": 60, "reason": "r", "noop": true})
            )
            .await
        ),
        "`prompt` is required when `stop` is not true."
    );
    assert_eq!(
        message(
            call(
                &tool,
                json!({"delaySeconds": 60, "reason": "r", "prompt": "p"})
            )
            .await
        ),
        "`noop` is required when `stop` is not true."
    );
}
