//! command hook 经执行环境运行（spec §5.1）。

use std::sync::Arc;

use rust_cli_kernel_api::ids::{EnvironmentId, Generation, OwnerEpoch, SessionId};

use super::*;
use crate::test_support::{Script, ScriptedEnv, script, workspace};

fn hook(command: &str) -> CommandHook {
    CommandHook {
        command: command.to_owned(),
        args: None,
        shell: HookShell::Bash,
        timeout: None,
        condition: None,
        background: false,
        plugin_root: None,
    }
}

fn context(operation: &str) -> OperationContext {
    OperationContext {
        environment: EnvironmentId::new("local"),
        generation: Generation::new("g1"),
        owner_epoch: OwnerEpoch::LOCAL,
        operation: OperationId::new(operation),
        origin: rust_cli_kernel_api::env::Origin::User {
            session: Some(SessionId::new("sess_1")),
        },
    }
}

async fn run_with(
    env: &ScriptedEnv,
    hook: &CommandHook,
    timeout_ms: u64,
    cancel: CancellationToken,
) -> Raw {
    let workspace = workspace();
    run(
        CommandRun {
            env,
            workspace: &workspace,
            operation: context("op_1"),
            hook,
            input: "{\"hook_event_name\":\"Stop\"}",
            timeout_ms,
        },
        cancel,
    )
    .await
}

#[tokio::test]
async fn sends_stdin_env_and_collects_separate_streams() {
    let env = ScriptedEnv::with(&[("guard.sh", script("out", "err", 2))]);
    let raw = run_with(&env, &hook("guard.sh"), 5_000, CancellationToken::new()).await;
    assert_eq!(
        raw,
        Raw::Command {
            stdout: "out".to_owned(),
            stderr: "err".to_owned(),
            exit: 2
        }
    );
    let (context, request) = env.started().remove(0);
    assert_eq!(context.operation.as_str(), "op_1");
    assert_eq!(request.cwd.as_str(), "/Users/dev/project");
    assert_eq!(request.timeout_ms, Some(5_000));
    assert_eq!(request.cwd_scope, CwdScope::Pinned);
    assert!(request.task.as_str().starts_with('h') && request.task.as_str().len() == 9);
    let io = request.hook.unwrap();
    assert_eq!(io.stdin, "{\"hook_event_name\":\"Stop\"}\n");
    // 名字按 Node（`createPluginEnvOverlay`，两套都给，修复线 F42，config.md §2.5.1）。
    for name in ["ZCODE_PROJECT_DIR", "CLAUDE_PROJECT_DIR"] {
        assert_eq!(
            io.env.get(name).map(String::as_str),
            Some("/Users/dev/project"),
            "{name}"
        );
    }
    for name in [
        "ZCODE_SESSION_ID",
        "CLAUDE_SESSION_ID",
        "CLAUDE_CODE_SESSION_ID",
    ] {
        assert_eq!(
            io.env.get(name).map(String::as_str),
            Some("sess_1"),
            "{name}"
        );
    }
    assert!(!io.env.contains_key("CLAUDE_PLUGIN_ROOT"));
}

/// exec 形式的参数里的会话 ID 变量也展开（Node `expandPluginVariables`，F42）。
#[test]
fn exec_form_expands_the_session_id() {
    let exec = CommandHook {
        args: Some(vec![
            "${ZCODE_SESSION_ID}".to_owned(),
            "${CLAUDE_SESSION_ID}:${CLAUDE_CODE_SESSION_ID}".to_owned(),
        ]),
        ..hook("log")
    };
    assert_eq!(
        command_line(&exec, "/w", Some("sess_1")),
        "'log' 'sess_1' 'sess_1:sess_1'"
    );
    // 没有会话时原样留下。
    assert_eq!(
        command_line(&exec, "/w", None),
        "'log' '${ZCODE_SESSION_ID}' '${CLAUDE_SESSION_ID}:${CLAUDE_CODE_SESSION_ID}'"
    );
}

#[tokio::test]
async fn timeout_and_cancel_are_cancelled() {
    let slow = Script {
        delay_ms: 10_000,
        ..Script::default()
    };
    let env = Arc::new(ScriptedEnv::with(&[("slow", slow)]));
    assert_eq!(
        run_with(&env, &hook("slow"), 20, CancellationToken::new()).await,
        Raw::Cancelled
    );
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    let slow_hook = hook("slow");
    let (raw, ()) = tokio::join!(run_with(&env, &slow_hook, 60_000, cancel), async move {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        trigger.cancel();
    });
    assert_eq!(raw, Raw::Cancelled);
}

#[tokio::test]
async fn start_failure_and_background() {
    let failing = ScriptedEnv::failing();
    assert_eq!(
        run_with(&failing, &hook("x"), 1_000, CancellationToken::new()).await,
        Raw::Failed("Failed to run: io error: spawn failed".to_owned())
    );
    let env = ScriptedEnv::default();
    let background = CommandHook {
        background: true,
        ..hook("bg")
    };
    assert_eq!(
        run_with(&env, &background, 1_000, CancellationToken::new()).await,
        Raw::Backgrounded
    );
    assert_eq!(env.started().len(), 1);
}

#[test]
fn exec_form_quotes_each_part() {
    let exec = CommandHook {
        args: Some(vec![
            "${CLAUDE_PROJECT_DIR}/a b.js".to_owned(),
            "it's".to_owned(),
        ]),
        ..hook("node")
    };
    assert_eq!(
        command_line(&exec, "/w", None),
        r"'node' '/w/a b.js' 'it'\''s'"
    );
    assert_eq!(
        command_line(&hook("echo $ZCODE_PROJECT_DIR"), "/w", None),
        "echo $ZCODE_PROJECT_DIR"
    );
    assert_eq!(
        task_id(&OperationId::new("op_1")),
        task_id(&OperationId::new("op_1"))
    );
    assert_ne!(
        task_id(&OperationId::new("op_1")),
        task_id(&OperationId::new("op_2"))
    );
}

/// 插件的 hook：命令与参数里的插件目录变量替换，环境变量两套名字都给（hooks.md §2.2 第 7 条）。
#[tokio::test]
async fn plugin_hooks_see_their_root() {
    let root = "/Users/dev/plugins/kit";
    let shell = CommandHook {
        plugin_root: Some(root.to_owned()),
        ..hook("${CLAUDE_PLUGIN_ROOT}/a.sh ${ZCODE_PLUGIN_ROOT}")
    };
    assert_eq!(
        command_line(&shell, "/w", None),
        "/Users/dev/plugins/kit/a.sh /Users/dev/plugins/kit"
    );
    let exec = CommandHook {
        args: Some(vec!["${CLAUDE_PLUGIN_ROOT}/b.js".to_owned()]),
        ..shell.clone()
    };
    assert_eq!(
        command_line(&exec, "/w", None),
        "'/Users/dev/plugins/kit/a.sh /Users/dev/plugins/kit' '/Users/dev/plugins/kit/b.js'"
    );
    let env = ScriptedEnv::with(&[(
        "/Users/dev/plugins/kit/a.sh /Users/dev/plugins/kit",
        script("", "", 0),
    )]);
    run_with(&env, &shell, 5_000, CancellationToken::new()).await;
    let io = env.started().remove(0).1.hook.unwrap();
    for name in ["CLAUDE_PLUGIN_ROOT", "ZCODE_PLUGIN_ROOT"] {
        assert_eq!(io.env.get(name).map(String::as_str), Some(root), "{name}");
    }
}

/// F11（R2-18）：hook 的 stdout 与 stderr 在内存里合计最多 8 MiB（hooks.md §5.1、D16），先到先留；旧代码全部攒下。
#[tokio::test]
async fn hook_output_in_memory_is_capped() {
    let stdout = "o".repeat(6 * 1024 * 1024);
    let stderr = "e".repeat(3 * 1024 * 1024);
    let env = ScriptedEnv::with(&[("loud.sh", script(&stdout, &stderr, 0))]);
    let raw = run_with(&env, &hook("loud.sh"), 5_000, CancellationToken::new()).await;
    let Raw::Command {
        stdout: kept_out,
        stderr: kept_err,
        exit,
    } = raw
    else {
        panic!("not a command result");
    };
    assert_eq!(exit, 0);
    assert_eq!(kept_out, stdout);
    assert_eq!(kept_err.len(), 2 * 1024 * 1024);
    assert_eq!(kept_out.len() + kept_err.len(), 8 * 1024 * 1024);
}
