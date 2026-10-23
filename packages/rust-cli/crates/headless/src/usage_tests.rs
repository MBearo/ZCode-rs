//! `-p "/usage"`（usage.md §6）。
use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::Kernel;
use rust_cli_kernel_api::store::SessionFilter;
use rust_cli_kernel_api::testing::{
    FixedClock, MemoryStore, ScriptedModel, SequentialIds, UnusedEnvironment, selection,
    text_response,
};
use rust_cli_runtime::{Defaults, LocalKernel, Ports};

use crate::{Output, PrintOptions, SessionTarget, run};

fn kernel(model: ScriptedModel) -> Arc<dyn Kernel> {
    let store = Arc::new(MemoryStore::default());
    Arc::new(LocalKernel::new(
        Ports {
            model: Arc::new(model),
            env: Arc::new(UnusedEnvironment),
            store: store.clone(),
            artifacts: store,
            clock: Arc::new(FixedClock(0)),
            ids: Arc::new(SequentialIds::default()),
        },
        Defaults {
            selection: selection(),
            mode: PermissionMode::Default,
            max_turns: None,
            search_tools: false,
            task_tools: None,
            limits: Default::default(),
        },
    ))
}

fn options(prompt: &str, target: SessionTarget) -> PrintOptions {
    PrintOptions {
        session_id: None,
        prompt: prompt.to_owned(),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/work"),
        },
        selection: selection(),
        mode: PermissionMode::Default,
        target,
        fork: false,
        stop: tokio_util::sync::CancellationToken::new(),
        output: Output::default(),
        cancel: tokio_util::sync::CancellationToken::new(),
    }
}

#[test]
fn the_three_names_are_usage() {
    for prompt in ["/usage", " /cost ", "/stats extra"] {
        assert!(super::is_usage(prompt), "{prompt}");
    }
    for prompt in ["/usages", "usage", "/context"] {
        assert!(!super::is_usage(prompt), "{prompt}");
    }
}

#[tokio::test]
async fn a_new_session_prints_the_zero_summary_without_a_session_or_request() {
    // 模型没有剧本：发了请求就会失败。
    let kernel = kernel(ScriptedModel::new(Vec::new()));
    let mut output = Vec::new();
    let options = options("/cost", SessionTarget::New);
    run(kernel.clone(), options.clone(), &mut output)
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8(output).unwrap(),
        "Total duration (API):  0s\nTotal duration (wall): 0s\nTotal code changes:    0 lines added, 0 lines removed\nUsage:                 0 input, 0 output, 0 cache read, 0 cache write\n"
    );
    let sessions = kernel
        .list_sessions(SessionFilter {
            workspace: options.workspace,
            limit: 10,
        })
        .await
        .unwrap();
    assert!(sessions.is_empty());
}

#[tokio::test]
async fn continuing_a_session_reports_its_usage_by_model() {
    let kernel = kernel(ScriptedModel::new(vec![text_response(&["hi"])]));
    let mut output = Vec::new();
    run(
        kernel.clone(),
        options("hello", SessionTarget::New),
        &mut output,
    )
    .await
    .unwrap();
    let mut output = Vec::new();
    run(
        kernel.clone(),
        options("/usage", SessionTarget::Continue),
        &mut output,
    )
    .await
    .unwrap();
    let text = String::from_utf8(output).unwrap();
    assert!(text.starts_with("Total duration (API):  0s\n"), "{text}");
    assert!(
        text.ends_with("Usage by model:\n            scripted:  0 input, 0 output, 0 cache read, 0 cache write\n"),
        "{text:?}"
    );
}
