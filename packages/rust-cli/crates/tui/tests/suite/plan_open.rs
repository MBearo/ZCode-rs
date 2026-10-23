//! `/plan open`（docs/specs/rust-cli/tui-manage.md §8）：先切到 plan，有计划时用外部编辑器（L27 的端口）打开计划文件。
use std::sync::{Arc, Mutex};

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath};
use rust_cli_kernel_api::testing::{MemoryEnvironment, text_response};
use rust_cli_tui::{Editor, Services};

use super::support::{GatedModel, Setup, with_ui};

/// 记下打开的文件。
#[derive(Default)]
struct Opened(Mutex<Vec<String>>);

impl Editor for Opened {
    fn edit(&self, text: String) -> BoxFuture<'static, Result<String, String>> {
        async move { Ok(text) }.boxed()
    }

    fn open(&self, path: String) -> BoxFuture<'static, Result<(), String>> {
        self.0.lock().unwrap().push(path);
        async { Ok(()) }.boxed()
    }

    fn source(&self) -> Option<(String, String)> {
        None
    }
}

const PLAN: &str = "/Users/dev/project/.zcode/plans/plan-sess_1.md";

fn env() -> Arc<MemoryEnvironment> {
    Arc::new(MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new("/Users/dev/project"),
        platform: "darwin".into(),
        os_version: "x".into(),
        shell: "zsh".into(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    }))
}

fn idle(screen: &str, text: &str) -> bool {
    screen.contains(text) && !screen.contains("esc to interrupt")
}

#[tokio::test(flavor = "multi_thread")]
async fn plan_open_switches_to_plan_and_opens_the_plan_file() {
    let model = GatedModel::scripted(vec![text_response(&["ok"])]);
    let editor = Arc::new(Opened::default());
    let env = env();
    let setup = Setup {
        env: env.clone(),
        services: Services {
            editor: Some(editor.clone()),
            ..Services::default()
        },
        ..Setup::default()
    };
    let session = with_ui(model, setup, |mut ui| async move {
        // 没有会话：只切到 plan。
        ui.submit("/plan open");
        ui.wait_for("Enabled plan mode").await;
        ui.wait_for("Plan mode").await;
        ui.submit("hello");
        ui.wait("the reply", |s| idle(s, "⏺ ok")).await;
        // 已经在 plan，还没有计划。
        ui.submit("/plan open");
        ui.wait_for("Already in plan mode. No plan written yet.")
            .await;
        env.set_file(PLAN, "# Plan\n");
        ui.submit("/plan open");
        ui.wait_for(&format!("Opened plan in editor: {PLAN}")).await;
    })
    .await;
    assert_eq!(session.as_deref(), Some("sess_1"));
    assert_eq!(*editor.0.lock().unwrap(), [PLAN]);
}
