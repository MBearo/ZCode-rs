//! command hook 的真实进程链路（docs/specs/rust-cli/hooks.md §5.1、§6）：真实 env-local（外套序列化
//! 往返）+ HookEngine + 执行管线 + Bash 工具，hook 脚本写在临时目录里。
#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    reason = "集成测试：辅助函数不在 #[test] 里，clippy 的测试豁免覆盖不到"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rust_cli_env_local::{LocalEnvironment, ShellConfig};
use rust_cli_hooks::{HookEngine, HookPorts, HookSource, HooksConfig, HooksSettings};
use rust_cli_kernel_api::env::{EnvPath, OperationContext, Origin, WorkspaceRef};
use rust_cli_kernel_api::hooks::{HookContext, HookEvent, HookEventName, HookRunner};
use rust_cli_kernel_api::ids::{
    EnvironmentId, Generation, InteractionId, OperationId, OwnerEpoch, RunId, SessionId, ToolUseId,
};
use rust_cli_kernel_api::message::{Message, ToolResultContent};
use rust_cli_kernel_api::orchestration::{
    InteractionAnswer, InteractionRequest, Interactions, OrchestrationError, ToolExecution,
    ToolExecutor,
};
use rust_cli_kernel_api::permission::{
    PermissionMode, PermissionRules, RuleBehavior, RuleSource, SourcedRule,
};
use rust_cli_kernel_api::testing::{
    MemoryArtifacts, MemoryStore, RoundTripEnv, SequentialIds, local_services, machine,
};
use rust_cli_kernel_api::tool::ToolUseContext;
use rust_cli_policy::PolicyGate;
use rust_cli_policy::rules::parse_rule;
use rust_cli_tools::{BashTool, Pipeline};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct NoOneToAsk;

#[async_trait]
impl Interactions for NoOneToAsk {
    async fn ask(
        &self,
        _session: &SessionId,
        _request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        Ok(InteractionAnswer {
            interaction: InteractionId::new("i"),
            answer: json!({"decision": "unavailable"}),
        })
    }
}

const GUARD: &str = r#"input=$(cat)
case "$input" in
  *'"command":"rm '*) echo "rm is blocked" >&2; exit 2 ;;
esac
exit 0
"#;

const CONTEXT: &str = r#"cat > /dev/null
printf '{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"dir=%s pwd=%s"}}' "$ZCODE_PROJECT_DIR" "$(pwd -P)"
"#;

struct Harness {
    _dir: tempfile::TempDir,
    root: PathBuf,
    scripts: PathBuf,
    env: Arc<RoundTripEnv<LocalEnvironment>>,
}

#[expect(clippy::disallowed_methods, reason = "测试准备工作区与 hook 脚本")]
fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let root = base.join("work");
    let scripts = base.join("hooks");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();
    std::fs::write(scripts.join("guard.sh"), GUARD).unwrap();
    std::fs::write(scripts.join("context.sh"), CONTEXT).unwrap();
    let shell = ShellConfig {
        scratch: base.join("scratch"),
        ledger: base.join("ledger"),
        bash: Some(PathBuf::from("/bin/bash")),
        bwrap: None,
        embedded: None,
        env: vec![(
            "PATH".to_owned(),
            "/usr/bin:/bin:/usr/sbin:/sbin".to_owned(),
        )]
        .into(),
    };
    let local = LocalEnvironment::new(None).with_shell(shell);
    Harness {
        _dir: dir,
        root,
        scripts,
        env: Arc::new(RoundTripEnv(Arc::new(local))),
    }
}

fn script(harness: &Harness, name: &str) -> String {
    format!("bash {}", harness.scripts.join(name).display())
}

fn engine(harness: &Harness, settings: &Value) -> HookEngine {
    let (parsed, warnings) = HooksSettings::from_json(settings);
    assert!(warnings.is_empty(), "{warnings:?}");
    HookEngine::with_config(
        HookPorts {
            commands: harness.env.clone(),
            shell: harness.env.clone(),
            ids: Arc::new(SequentialIds::default()),
            model: None,
            http: None,
            generation: Generation::new("g1"),
            artifacts: Arc::new(MemoryArtifacts::default()),
        },
        HooksConfig {
            version: 1,
            sources: vec![(HookSource::Project, parsed)],
            ..HooksConfig::default()
        },
    )
}

fn workspace(root: &Path) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(root.to_string_lossy()),
    }
}

fn context(harness: &Harness, index: usize) -> ToolUseContext {
    let session = SessionId::new("sess_1");
    let env = harness.env.clone();
    ToolUseContext {
        session: session.clone(),
        run: RunId::new("run_1"),
        tool_use: ToolUseId::new(format!("toolu_{index}")),
        agent: None,
        workspace: workspace(&harness.root),
        mode: PermissionMode::DontAsk,
        cancel: CancellationToken::new(),
        machine: machine(env.clone()),
        local: local_services(env),
        artifacts: Arc::new(MemoryStore::default()),
        interactions: Arc::new(NoOneToAsk),
        operation: OperationContext {
            environment: EnvironmentId::new("local"),
            generation: Generation::new("g1"),
            owner_epoch: OwnerEpoch::LOCAL,
            operation: OperationId::new(format!("op_tool_{index}")),
            origin: Origin::Agent {
                session,
                run: RunId::new("run_1"),
            },
        },
    }
}

fn text(content: &[ToolResultContent]) -> String {
    match content {
        [ToolResultContent::Text { text }] => text.clone(),
        other => format!("{other:?}"),
    }
}

fn attachment_texts(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .map(|message| match message {
            Message::Attachment(attachment) => attachment.text.clone(),
            other => format!("{other:?}"),
        })
        .collect()
}

async fn bash(
    pipeline: &Pipeline,
    harness: &Harness,
    index: usize,
    command: &str,
) -> ToolExecution {
    let (progress, _updates) = mpsc::channel(64);
    pipeline
        .execute_full(
            Arc::new(BashTool::new()),
            json!({"command": command}),
            context(harness, index),
            progress,
        )
        .await
}

#[tokio::test]
async fn pre_and_post_tool_hooks_run_as_real_processes() {
    let harness = harness();
    let settings = json!({"hooks": {
        "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": script(&harness, "guard.sh")}]}],
        "PostToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": script(&harness, "context.sh")}]}]
    }});
    let hooks = Arc::new(engine(&harness, &settings));
    let gate = PolicyGate::new(PermissionRules {
        version: 1,
        rules: vec![SourcedRule {
            source: RuleSource::CliArg,
            behavior: RuleBehavior::Allow,
            rule: parse_rule("Bash"),
        }],
        ..PermissionRules::default()
    });
    let pipeline = Pipeline::new(Arc::new(gate), hooks);

    let blocked = bash(&pipeline, &harness, 1, "rm -rf data").await;
    assert!(blocked.output.is_error);
    assert_eq!(
        text(&blocked.output.content),
        format!(
            "PreToolUse:Bash hook error: [{}]: rm is blocked\n",
            script(&harness, "guard.sh")
        )
    );
    assert!(blocked.after.is_empty());

    let ran = bash(&pipeline, &harness, 2, "echo hi").await;
    assert!(!ran.output.is_error);
    assert_eq!(text(&ran.output.content), "hi");
    let root = harness.root.display().to_string();
    assert_eq!(
        attachment_texts(&ran.after),
        [format!(
            "<system-reminder>\nPostToolUse:Bash hook additional context: dir={root} pwd={root}\n</system-reminder>"
        )]
    );
    assert!(!ran.prevent_continuation);
}

#[tokio::test]
async fn slow_hooks_time_out_and_exit_two_blocks_stop() {
    let harness = harness();
    let settings = json!({"hooks": {"Stop": [{"hooks": [
        {"type": "command", "command": "sleep 5", "timeout": 1},
        {"type": "command", "command": "echo 'run the tests' >&2; exit 2"}
    ]}]}});
    let engine = engine(&harness, &settings);
    let started = Instant::now();
    let outcome = engine
        .run(
            HookEvent {
                name: HookEventName::Stop,
                session: SessionId::new("sess_1"),
                workspace: workspace(&harness.root),
                payload: json!({"stop_hook_active": false, "background_tasks": [], "session_crons": []}),
                context: HookContext {
                    mode: Some(PermissionMode::Default),
                    ..HookContext::default()
                },
            },
            CancellationToken::new(),
        )
        .await;
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "超时的 hook 被结束"
    );
    let feedback: Vec<String> = outcome
        .messages
        .iter()
        .map(|message| match message {
            Message::User(user) => format!("{:?}", user.content),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(feedback.len(), 1);
    assert!(
        feedback[0]
            .contains("Stop hook feedback:\\n[echo 'run the tests' >&2; exit 2]: run the tests\\n"),
        "{feedback:?}"
    );
    assert!(!outcome.prevent_continuation);
}
