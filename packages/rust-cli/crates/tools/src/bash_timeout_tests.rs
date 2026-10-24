//! 前台命令超时转到后台（docs/specs/rust-cli/shell-permissions.md §1.2.1）：哪些命令请执行环境转、转后台之后的期限、
//! 结果的原文。

use rust_cli_kernel_api::env_shell::{BackgroundCommand, TimeoutBackground};

use super::*;

/// 到了超时、转到后台的执行环境。
fn moving() -> Arc<Scripted> {
    Arc::new(Scripted {
        output: b"start\n".to_vec(),
        backgrounded: true,
        ..Scripted::default()
    })
}

async fn call_with(
    tool: BashTool,
    env: Arc<Scripted>,
    input: Value,
) -> Result<ToolOutput, ToolError> {
    let (progress, _updates) = mpsc::channel(8);
    tool.call(input, context(env, None), progress).await
}

fn task() -> String {
    output::task_id(&OperationId::new("op_1")).to_string()
}

#[tokio::test]
async fn a_timed_out_foreground_command_moves_to_the_background() {
    let env = moving();
    let output = call_with(
        BashTool::new(),
        env.clone(),
        json!({"command": "npm test", "timeout": 1000}),
    )
    .await
    .unwrap();
    assert!(!output.is_error);
    assert_eq!(
        text_of(&output),
        format!(
            "Command did not complete within its 1s timeout and was moved to the background (ID: {}). Output is being written to: /t/b.output. You will be notified when it completes. If it is still running after 10m in the background, it will be stopped and you will be notified. To check interim output, use Read on that file path.",
            task()
        )
    );
    let start = env.started.lock().unwrap()[0].clone();
    assert_eq!(start.timeout_ms, Some(1000));
    assert!(start.background.is_none());
    assert_eq!(
        start.on_timeout,
        Some(TimeoutBackground {
            command: BackgroundCommand {
                description: "npm test".to_owned(),
                tool_use: Some(ToolUseId::new("toolu_1")),
                kind: Default::default(),
            },
            deadline_ms: Some(600_000),
        })
    );
}

/// 第一个子命令的第一个词是 `sleep` 时照旧按超时结束（不请执行环境转后台）；别的写法都转。后台命令本来就在后台。
#[tokio::test]
async fn only_commands_not_starting_with_sleep_move() {
    let cases = [
        ("sleep 3; echo late", false),
        ("  sleep 3", false),
        ("sleep 3 && echo late", false),
        ("sleep", false),
        ("echo a; sleep 3", true),
        ("FOO=1 sleep 3", true),
        ("(sleep 3)", true),
        ("sleepy 3", true),
        ("cargo test", true),
    ];
    for (command, moves) in cases {
        let env = Arc::new(Scripted::default());
        call_with(BashTool::new(), env.clone(), json!({"command": command}))
            .await
            .unwrap();
        let start = env.started.lock().unwrap()[0].clone();
        assert_eq!(start.on_timeout.is_some(), moves, "{command}");
    }
    let env = Arc::new(Scripted::default());
    call_with(
        BashTool::new(),
        env.clone(),
        json!({"command": "cargo test", "run_in_background": true}),
    )
    .await
    .unwrap();
    assert!(env.started.lock().unwrap()[0].on_timeout.is_none());
}

/// 交互会话的后台命令没有期限：转后台之后也没有，说明里没有 “If it is still running …” 一句；stream-json 输入的
/// 期限是 30 分钟。超时按秒四舍五入，至少 1 s。
#[tokio::test]
async fn the_deadline_follows_the_background_deadline() {
    let env = moving();
    let output = call_with(
        BashTool::new().with_background_deadline(false),
        env.clone(),
        json!({"command": "npm test", "timeout": 1500}),
    )
    .await
    .unwrap();
    assert_eq!(
        text_of(&output),
        format!(
            "Command did not complete within its 2s timeout and was moved to the background (ID: {}). Output is being written to: /t/b.output. You will be notified when it completes. To check interim output, use Read on that file path.",
            task()
        )
    );
    let start = env.started.lock().unwrap()[0].clone();
    assert_eq!(start.on_timeout.unwrap().deadline_ms, None);
    let env = moving();
    let output = call_with(
        BashTool::new().with_streaming_input(),
        env.clone(),
        json!({"command": "npm test", "timeout": 400}),
    )
    .await
    .unwrap();
    assert!(
        text_of(&output).starts_with("Command did not complete within its 1s timeout"),
        "{}",
        text_of(&output)
    );
    assert!(text_of(&output).contains("If it is still running after 30m in the background"));
    let start = env.started.lock().unwrap()[0].clone();
    assert_eq!(start.on_timeout.unwrap().deadline_ms, Some(1_800_000));
}
