//! Monitor 的事件在会话 actor 里（tools-more.md §19.6）：批成一条事件通知开回合、命令结束时的终态通知带剩下的行、
//! 到期说明并停掉命令、被停掉时剩下的行作为普通事件。执行环境是替身：输出流与终态通知由测试推入。

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, EnvPath, FileContent, OperationContext, Outcome,
    OutputStream, ReadFile, StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use rust_cli_kernel_api::env_shell::{
    BackgroundInfo, BackgroundKind, BackgroundStatus, BackgroundStop, BackgroundTask, ShellNotice,
    ShellState,
};
use rust_cli_kernel_api::ids::{OperationId, ShellTaskId, ToolUseId};
use rust_cli_kernel_api::local_services::{Fetch, Git};
use rust_cli_kernel_api::machine::{
    Background, Checkpoints, Commands, EnvInfo, Files, Pdf, Search, Shell,
};
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::monitor::{MonitorArmed, MonitorSource};
use rust_cli_kernel_api::testing::{NoticeEnvironment, shell_notice, tool_use_response};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::*;

type Events = mpsc::UnboundedSender<Result<CommandEvent, EnvError>>;

/// 执行环境替身：每个操作一条可推入的输出流；终态通知经 [`NoticeEnvironment`]。
#[derive(Default)]
struct MonitorEnv {
    notices: NoticeEnvironment,
    streams: Mutex<HashMap<OperationId, mpsc::UnboundedReceiver<Result<CommandEvent, EnvError>>>>,
    running: Mutex<HashSet<ShellTaskId>>,
    /// 已结束、通知还没 ack 的（同 env-local：先记通知，再报告结束）。
    ended: Mutex<HashMap<ShellTaskId, u64>>,
    stopped: Mutex<Vec<ShellTaskId>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap()
}

impl MonitorEnv {
    fn command(&self, operation: &str, task: &str) -> Events {
        let (sender, receiver) = mpsc::unbounded_channel();
        lock(&self.streams).insert(OperationId::new(operation), receiver);
        lock(&self.running).insert(ShellTaskId::new(task));
        sender
    }

    fn end(&self, notice: ShellNotice) {
        lock(&self.running).remove(&notice.task);
        lock(&self.ended).insert(notice.task.clone(), notice.seq);
        self.notices.finish(notice);
    }

    fn stopped(&self) -> Vec<ShellTaskId> {
        lock(&self.stopped).clone()
    }
}

fn unused() -> EnvError {
    EnvError::Unsupported {
        what: "not in this test".to_owned(),
    }
}

#[async_trait]
impl EnvInfo for MonitorEnv {
    async fn facts(&self, _workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        Err(unused())
    }
}

#[async_trait]
impl Files for MonitorEnv {
    async fn read_file(&self, _w: &WorkspaceRef, _r: ReadFile) -> Result<FileContent, EnvError> {
        Err(unused())
    }

    async fn write_file(
        &self,
        _w: &WorkspaceRef,
        _c: OperationContext,
        _r: WriteFile,
    ) -> Result<WriteReceipt, EnvError> {
        Err(unused())
    }
}

#[async_trait]
impl Commands for MonitorEnv {
    async fn start_command(
        &self,
        _w: &WorkspaceRef,
        _c: OperationContext,
        _r: StartCommand,
    ) -> Result<(), EnvError> {
        Err(unused())
    }

    fn watch_command(
        &self,
        operation: OperationId,
        _cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        match lock(&self.streams).remove(&operation) {
            Some(mut receiver) => stream::poll_fn(move |cx| receiver.poll_recv(cx)).boxed(),
            None => stream::pending().boxed(),
        }
    }

    async fn cancel(&self, _operation: OperationId) -> Result<(), EnvError> {
        Err(unused())
    }

    async fn operation_status(&self, _operation: OperationId) -> Result<Outcome, EnvError> {
        Err(unused())
    }

    async fn close_session(
        &self,
        session: &rust_cli_kernel_api::ids::SessionId,
        forget: bool,
    ) -> Result<(), EnvError> {
        self.notices.close_session(session, forget).await
    }
}

#[async_trait]
impl Shell for MonitorEnv {
    async fn shell_state(
        &self,
        workspace: &WorkspaceRef,
        session: &rust_cli_kernel_api::ids::SessionId,
    ) -> Result<ShellState, EnvError> {
        let mut state = self.notices.shell_state(workspace, session).await?;
        state.background.extend(lock(&self.running).iter().cloned());
        Ok(state)
    }

    fn shell_notices(
        &self,
        session: &rust_cli_kernel_api::ids::SessionId,
        after: u64,
    ) -> BoxStream<'static, Result<ShellNotice, EnvError>> {
        self.notices.shell_notices(session, after)
    }

    async fn ack_shell_notice(
        &self,
        session: &rust_cli_kernel_api::ids::SessionId,
        seq: u64,
    ) -> Result<(), EnvError> {
        self.notices.ack_shell_notice(session, seq).await
    }
}

#[async_trait]
impl Background for MonitorEnv {
    async fn background_command(
        &self,
        request: BackgroundTask,
    ) -> Result<Option<BackgroundInfo>, EnvError> {
        let status = if lock(&self.running).contains(&request.task) {
            BackgroundStatus::Running
        } else if lock(&self.ended)
            .get(&request.task)
            .is_some_and(|seq| !self.notices.acked().contains(seq))
        {
            BackgroundStatus::Completed
        } else {
            return Ok(None);
        };
        Ok(Some(BackgroundInfo {
            command: "watch".to_owned(),
            status,
        }))
    }

    async fn stop_background_command(
        &self,
        _context: OperationContext,
        request: BackgroundTask,
    ) -> Result<BackgroundStop, EnvError> {
        lock(&self.running).remove(&request.task);
        lock(&self.stopped).push(request.task);
        Ok(BackgroundStop::Stopped {
            command: "watch".to_owned(),
        })
    }
}

impl Search for MonitorEnv {}
impl Checkpoints for MonitorEnv {}
impl Pdf for MonitorEnv {}
impl Git for MonitorEnv {}
impl Fetch for MonitorEnv {}

/// 走着的时钟（monitor 的批与到期按时钟算）。
struct Ticking(tokio::time::Instant);

impl rust_cli_kernel_api::clock::Clock for Ticking {
    fn now_ms(&self) -> u64 {
        1_000_000 + u64::try_from(self.0.elapsed().as_millis()).unwrap()
    }
    fn time_zone(&self) -> String {
        "UTC".to_owned()
    }
}

/// 交出 monitor 的工具（替 Monitor 工具调用 `arm_monitor`）。
struct ArmTool;

#[async_trait]
impl Tool for ArmTool {
    fn name(&self) -> &str {
        "Arm"
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }
    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let armed = MonitorArmed {
            operation: OperationId::new(input["operation"].as_str().unwrap()),
            task: ShellTaskId::new(input["task"].as_str().unwrap()),
            tool_use: context.tool_use.clone(),
            description: input["description"].as_str().unwrap().to_owned(),
            timeout_ms: input["timeout_ms"].as_u64().unwrap(),
            source: if input["socket"] == json!(true) {
                MonitorSource::Socket
            } else {
                MonitorSource::Command
            },
            output: EnvPath::new("/t/b1.output"),
        };
        context
            .interactions
            .arm_monitor(&context.session, armed)
            .await
            .map_err(|error| ToolError::Failed {
                message: error.to_string(),
            })?;
        Ok(rust_cli_kernel_api::tool::ToolOutput {
            content: vec![rust_cli_kernel_api::message::ToolResultContent::Text {
                text: "armed".to_owned(),
            }],
            is_error: false,
            display: Value::Null,
        })
    }
}

fn arm(
    timeout_ms: u64,
) -> Vec<Result<rust_cli_kernel_api::model::ModelEvent, rust_cli_kernel_api::model::ModelError>> {
    tool_use_response(
        "toolu_1",
        "Arm",
        &json!({"operation": "op_m", "task": "b1", "description": "errors <log>", "timeout_ms": timeout_ms}),
    )
}

fn output(data: &str, cursor: u64) -> Result<CommandEvent, EnvError> {
    Ok(CommandEvent::Output {
        stream: OutputStream::Stdout,
        data: data.as_bytes().to_vec(),
        cursor,
    })
}

fn finished(exit_code: i32) -> Result<CommandEvent, EnvError> {
    Ok(CommandEvent::Finished {
        outcome: Outcome::Completed,
        exit_code: Some(exit_code),
        timed_out: false,
        cwd_reset: None,
        output_bytes: 0,
        output_limit: None,
    })
}

fn monitor_notice(output_bytes: u64) -> ShellNotice {
    let mut notice = shell_notice(1, "b1");
    notice.kind = BackgroundKind::Monitor;
    notice.output_bytes = output_bytes;
    notice.tool_use = Some(ToolUseId::new("toolu_1"));
    notice.description = "errors <log>".to_owned();
    notice
}

async fn session_with(
    scope: &TaskScope,
    env: Arc<MonitorEnv>,
    model: Arc<ScriptedModel>,
) -> Session {
    super::notice::session_on(
        scope,
        env,
        model,
        vec![Arc::new(ArmTool)],
        Vec::new(),
        Arc::new(Ticking(tokio::time::Instant::now())),
    )
    .await
}

/// 模型收到的最后一条 user 消息的文字。
fn last_user(model: &ScriptedModel) -> String {
    let requests = model.requests();
    let request = requests.last().unwrap();
    format!("{:?}", request.messages.last().unwrap())
}

async fn turns(events: &mut BoxStream<'static, SequencedEvent>, count: usize) {
    until_finished(events, count).await;
}

#[tokio::test]
async fn lines_arrive_as_one_event_notice_after_the_batch_delay() {
    let scope = TaskScope::root("test", |_| {});
    let env = Arc::new(MonitorEnv::default());
    let output_events = env.command("op_m", "b1");
    let model = Arc::new(ScriptedModel::new(vec![
        arm(60_000),
        text_response(&["armed"]),
        text_response(&["saw it"]),
    ]));
    let session = session_with(&scope, env.clone(), model.clone()).await;
    let mut events = session.subscribe(Some(0));
    session.send(submit("c1", "watch")).await;
    turns(&mut events, 1).await;
    output_events
        .send(output("one\ntwo <b>\npart", 16))
        .unwrap();
    turns(&mut events, 1).await;
    let text = last_user(&model);
    assert!(text.contains("<task-id>b1</task-id>"), "{text}");
    assert!(
        text.contains("<summary>Monitor event: \\\"errors &lt;log&gt;\\\"</summary>"),
        "{text}"
    );
    assert!(
        text.contains("<event>one\\ntwo &lt;b&gt;</event>"),
        "{text}"
    );
    assert!(!text.contains("part"), "{text}");
}

#[tokio::test]
async fn the_end_notice_carries_the_rest_and_is_acked_after_commit() {
    let scope = TaskScope::root("test", |_| {});
    let env = Arc::new(MonitorEnv::default());
    let output_events = env.command("op_m", "b1");
    let model = Arc::new(ScriptedModel::new(vec![
        arm(60_000),
        text_response(&["armed"]),
        text_response(&["ended"]),
    ]));
    let session = session_with(&scope, env.clone(), model.clone()).await;
    let mut events = session.subscribe(Some(0));
    session.send(submit("c1", "watch")).await;
    turns(&mut events, 1).await;
    // 终态通知先到、事件流后结束：两者都到了才合成。
    env.end(monitor_notice(8));
    output_events.send(output("one\ntwo", 7)).unwrap();
    output_events.send(finished(0)).unwrap();
    turns(&mut events, 1).await;
    let text = last_user(&model);
    assert!(
        text.contains("<task-id>b1</task-id>\\n<tool-use-id>toolu_1</tool-use-id>\\n<output-file>/tmp/b1.output</output-file>\\n<status>completed</status>\\n<summary>Monitor \\\"errors &lt;log&gt;\\\" stream ended</summary>\\n<event>one\\ntwo</event>\\n</task-notification>"),
        "{text}"
    );
    assert_eq!(env.notices.acked(), [1]);
    assert_eq!(model.requests().len(), 3);
}

#[tokio::test]
async fn expiry_reports_and_stops_the_command() {
    let scope = TaskScope::root("test", |_| {});
    let env = Arc::new(MonitorEnv::default());
    let output_events = env.command("op_m", "b1");
    let model = Arc::new(ScriptedModel::new(vec![
        arm(1_000),
        text_response(&["armed"]),
        text_response(&["expired"]),
        text_response(&["late line"]),
    ]));
    let session = session_with(&scope, env.clone(), model.clone()).await;
    let mut events = session.subscribe(Some(0));
    session.send(submit("c1", "watch")).await;
    turns(&mut events, 1).await;
    output_events.send(output("half", 4)).unwrap();
    turns(&mut events, 1).await;
    assert!(
        last_user(&model).contains("[Monitor expired after 1s with no events delivered."),
        "{}",
        last_user(&model)
    );
    assert_eq!(env.stopped(), [ShellTaskId::new("b1")]);
    // 停掉之后流结束：执行环境不认识它了，剩下的半行作为普通事件送出，没有终态通知。
    output_events.send(finished(137)).unwrap();
    turns(&mut events, 1).await;
    assert!(last_user(&model).contains("<event>half</event>"));
    assert!(env.notices.acked().is_empty());
}

#[tokio::test]
async fn a_notice_without_a_watch_uses_the_monitor_summary() {
    let notice = monitor_notice(0);
    let text = rust_cli_query::task_notification(&notice);
    assert!(
        text.contains("<summary>Monitor \"errors &lt;log&gt;\" ended without producing output (exit 0)</summary>\n</task-notification>"),
        "{text}"
    );
    let _ = Message::User;
}

fn arm_socket()
-> Vec<Result<rust_cli_kernel_api::model::ModelEvent, rust_cli_kernel_api::model::ModelError>> {
    tool_use_response(
        "toolu_1",
        "Arm",
        &json!({"operation": "op_m", "task": "s1", "description": "feed", "timeout_ms": 60_000, "socket": true}),
    )
}

fn lifecycle(line: &str) -> Result<CommandEvent, EnvError> {
    Ok(CommandEvent::Output {
        stream: OutputStream::Stderr,
        data: format!("{line}\n").into_bytes(),
        cursor: 0,
    })
}

/// ws 来源：关闭时先送出待发的帧，再送关闭说明；终态通知只 ack，不送 `<task-notification>` 终态。
#[tokio::test]
async fn a_socket_close_flushes_frames_then_reports_the_close() {
    let scope = TaskScope::root("test", |_| {});
    let env = Arc::new(MonitorEnv::default());
    let output_events = env.command("op_m", "s1");
    let model = Arc::new(ScriptedModel::new(vec![
        arm_socket(),
        text_response(&["armed"]),
        text_response(&["closed"]),
    ]));
    let session = session_with(&scope, env.clone(), model.clone()).await;
    let mut events = session.subscribe(Some(0));
    session.send(submit("c1", "watch")).await;
    turns(&mut events, 1).await;
    let mut notice = shell_notice(7, "s1");
    notice.kind = BackgroundKind::Monitor;
    env.end(notice);
    output_events.send(output("frame one\n", 10)).unwrap();
    output_events
        .send(lifecycle(r#"{"type":"closed","code":1000}"#))
        .unwrap();
    output_events.send(finished(0)).unwrap();
    turns(&mut events, 1).await;
    let requests = model.requests();
    let text = format!("{:?}", requests.last().unwrap().messages);
    let event = text.find("<event>frame one</event>").expect("the frame");
    let closed = text
        .find("<event>[WebSocket closed: 1000]</event>")
        .expect("the close");
    assert!(event < closed, "{text}");
    assert!(!text.contains("stream ended"), "{text}");
    assert_eq!(env.notices.acked(), [7]);
}

/// 用户停掉 ws 来源（执行环境不认识它了、没有生命周期）：送 `[Monitor stopped]`。
#[tokio::test]
async fn a_stopped_socket_reports_monitor_stopped() {
    let scope = TaskScope::root("test", |_| {});
    let env = Arc::new(MonitorEnv::default());
    let output_events = env.command("op_m", "s1");
    let model = Arc::new(ScriptedModel::new(vec![
        arm_socket(),
        text_response(&["armed"]),
        text_response(&["stopped"]),
    ]));
    let session = session_with(&scope, env.clone(), model.clone()).await;
    let mut events = session.subscribe(Some(0));
    session.send(submit("c1", "watch")).await;
    turns(&mut events, 1).await;
    lock(&env.running).remove(&ShellTaskId::new("s1"));
    output_events.send(finished(0)).unwrap();
    turns(&mut events, 1).await;
    assert!(last_user(&model).contains("<event>[Monitor stopped]</event>"));
}
