//! 用户调用 skill（docs/specs/rust-cli/ext.md §2.7、tui.md §6，工作线 L22）：斜杠补全列出 skill，
//! `/<名字> 参数` 由会话展开（模型收到命令标签与正文），用户行显示用户输入的样子；`user-invocable: false`
//! 不调模型，只显示提示。

use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvFacts, EnvPath};
use rust_cli_kernel_api::testing::{MemoryEnvironment, environments, text_response};

use super::support::{GatedModel, Setup, request_text, with_ui};

const GREET: &str = "---\ndescription: Greet someone by name\nargument-hint: <name>\n---\nSay hello to $ARGUMENTS.\n";
const INNER: &str = "---\ndescription: Model only\nuser-invocable: false\n---\nInner body.\n";

fn setup() -> Setup {
    let env = MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new("/Users/dev/project"),
        platform: "darwin".into(),
        os_version: "x".into(),
        shell: "zsh".into(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    })
    .with_file("/Users/dev/project/.zcode/skills/greet/SKILL.md", GREET)
    .with_file("/Users/dev/project/.zcode/skills/inner/SKILL.md", INNER);
    Setup {
        environments: environments(Arc::new(env)),
        ..Setup::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_skill_command_is_completed_expanded_and_shown_as_typed() {
    let model = GatedModel::scripted(vec![text_response(&["hello Ada"])]);
    with_ui(model.clone(), setup(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        // 补全：输入 `/gr` 时列出 skill 与它的说明；补全后显示参数提示。
        ui.type_text("/gr");
        ui.wait_for("Greet someone by name").await;
        ui.key(crossterm::event::KeyCode::Tab);
        ui.wait_for("/greet <name>").await;
        ui.submit("Ada");
        ui.wait("the reply", |s| {
            s.contains("hello Ada") && !s.contains("esc to interrupt")
        })
        .await;
        let screen = ui.screen();
        assert!(screen.contains("❯ /greet Ada"), "{screen}");
        assert!(!screen.contains("command-message"), "{screen}");
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    let text = request_text(&requests[0]);
    assert!(
        text.contains("<command-name>/greet</command-name>"),
        "{text}"
    );
    assert!(text.contains("Say hello to Ada."), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_builtin_loop_is_expanded_too() {
    let model = GatedModel::scripted(vec![text_response(&["looping"])]);
    with_ui(model.clone(), setup(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        // 清单在启动时取：等它到了再发（补全里出现 loop）。
        ui.type_text("/lo");
        ui.wait_for("Run a prompt or slash command").await;
        ui.press(
            crossterm::event::KeyCode::Char('u'),
            crossterm::event::KeyModifiers::CONTROL,
        );
        ui.submit("/loop 5m check the deploy");
        ui.wait("the reply", |s| {
            s.contains("looping") && !s.contains("esc to interrupt")
        })
        .await;
    })
    .await;
    let text = request_text(&model.requests()[0]);
    assert!(
        text.contains("<command-name>/loop</command-name>"),
        "{text}"
    );
    assert!(
        text.contains("<command-args>5m check the deploy</command-args>"),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_model_only_skill_answers_locally_and_unknown_names_stay_local() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(model.clone(), setup(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.type_text("/gr");
        ui.wait_for("Greet someone by name").await;
        ui.press(
            crossterm::event::KeyCode::Char('u'),
            crossterm::event::KeyModifiers::CONTROL,
        );
        ui.submit("/inner");
        ui.wait_for("This skill can only be invoked by ZCode").await;
        ui.submit("/nope");
        ui.wait_for("'/nope'").await;
    })
    .await;
    assert!(model.requests().is_empty());
}

/// 空闲时 esc 取消本会话的 loop 唤醒（tools-more.md §18.1）：之后模型停止循环时没有可取消的唤醒。
#[tokio::test(flavor = "multi_thread")]
async fn esc_while_idle_cancels_the_pending_loop_wakeup() {
    use rust_cli_kernel_api::testing::tool_use_response;
    let model = GatedModel::scripted(vec![
        tool_use_response(
            "toolu_1",
            "ScheduleWakeup",
            &serde_json::json!({
                "delaySeconds": 600,
                "reason": "waiting on the deploy",
                "prompt": "/loop check the deploy",
                "noop": false
            }),
        ),
        text_response(&["waiting"]),
        tool_use_response(
            "toolu_2",
            "ScheduleWakeup",
            &serde_json::json!({"stop": true}),
        ),
        text_response(&["stopped"]),
    ]);
    with_ui(model.clone(), setup(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("start the loop");
        ui.wait("the first turn", |s| {
            s.contains("waiting") && !s.contains("esc to interrupt")
        })
        .await;
        ui.key(crossterm::event::KeyCode::Esc);
        // 中断命令先到 actor，再发下一条输入（两条命令各自一个回执 future）。
        ui.settle().await;
        ui.submit("stop it");
        ui.wait("the second turn", |s| {
            s.contains("stopped") && !s.contains("esc to interrupt")
        })
        .await;
    })
    .await;
    let requests = model.requests();
    let text = request_text(&requests[3]);
    assert!(
        text.contains("there was no pending wakeup to cancel"),
        "{text}"
    );
}
