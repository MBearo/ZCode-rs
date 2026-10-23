//! next 通道（docs/specs/rust-cli/query.md §5，L18）：后台通知在下一个工具边界并入当前回合，与插话按
//! 到达顺序交错；回合结束后连续的通知合成一个回合，排在 `Queue` 输入之前；提交之后才 ack。

use rust_cli_kernel_api::message::{Message, UserOrigin};
use rust_cli_kernel_api::testing::{NoticeEnvironment, shell_notice, tool_use_response};
use rust_cli_kernel_api::tool::Tool;
use rust_cli_query::task_notification;

use super::notice::{notices_in, session};
use super::*;

fn steer(id: &str, text: &str) -> CommandEnvelope {
    CommandEnvelope {
        id: CommandId::new(id),
        command: Command::Submit {
            text: text.to_owned(),
            attachments: Vec::new(),
            delivery: Delivery::Steer,
        },
    }
}

/// 等到 actor 把这条后台工作排进通道（`BackgroundWork` 事件）。
async fn queued(events: &mut BoxStream<'static, SequencedEvent>, task: &str) {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .unwrap()
            .unwrap();
        if matches!(&event.event, KernelEvent::BackgroundWork { work, .. } if work == task) {
            return;
        }
    }
}

/// 工具边界并入的项（`queued_command`）：插话给原文，通知给任务 ID。
fn absorbed(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::Attachment(attachment) if attachment.source == "queued_command" => {
                let prompt = attachment.data["prompt"].as_str().unwrap_or_default();
                Some(match attachment.data["commandMode"].as_str() {
                    Some("task-notification") => {
                        let task = prompt.split("<task-id>").nth(1).unwrap_or_default();
                        format!("notice:{}", task.split('<').next().unwrap_or_default())
                    }
                    _ => format!("prompt:{prompt}"),
                })
            }
            _ => None,
        })
        .collect()
}

/// 第一次响应等放行（回合没有工具边界），之后交给剧本；记下全部请求。
struct GatedThenScripted {
    gate: Arc<Notify>,
    first: std::sync::atomic::AtomicBool,
    rest: ScriptedModel,
    requests: std::sync::Mutex<Vec<ModelRequest>>,
}

impl GatedThenScripted {
    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl ModelPort for GatedThenScripted {
    fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        self.requests.lock().unwrap().push(request.clone());
        if !self.first.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return self.rest.stream(request, cancel);
        }
        let gate = self.gate.clone();
        stream::once(async move { gate.notified().await })
            .flat_map(|()| stream::iter(text_response(&["ok"])))
            .boxed()
    }
}

async fn asking(
    scope: &TaskScope,
    env: Arc<NoticeEnvironment>,
    replies: Vec<Vec<Result<ModelEvent, ModelError>>>,
) -> (Arc<ScriptedModel>, Session) {
    let mut script = vec![tool_use_response(
        "toolu_1",
        "AskUserQuestion",
        &serde_json::json!({"question": "which?"}),
    )];
    script.extend(replies);
    let model = Arc::new(ScriptedModel::new(script));
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(super::AskingTool)];
    let session = session(scope, env, model.clone(), tools, Vec::new()).await;
    (model, session)
}

#[tokio::test]
async fn a_notice_during_a_tool_joins_the_turn_at_the_next_tool_boundary() {
    let scope = TaskScope::root("test", |_| {});
    let env = Arc::new(NoticeEnvironment::default());
    let (model, session) = asking(&scope, env.clone(), vec![text_response(&["thanks"])]).await;
    let mut events = session.subscribe(Some(0));
    session.send(submit("c1", "ask me")).await;
    let interaction = super::asked(&mut events).await;
    env.finish(shell_notice(1, "b1"));
    queued(&mut events, "b1").await;
    // 还没到工具边界：不 ack，仍算后台工作。
    assert!(env.acked().is_empty());
    session.send(super::answer("c2", interaction)).await;
    until_finished(&mut events, 1).await;
    // 通知折进工具结果之后的请求，不另开回合。
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    assert!(format!("{:?}", requests[1].messages).contains("<task-id>b1</task-id>"));
    let view = session.view().await.unwrap();
    assert_eq!(absorbed(&view.messages), ["notice:b1"]);
    assert!(notices_in(&view.messages).is_empty());
    assert_eq!(env.acked(), [1]);
    assert!(view.background.is_empty() && !view.running);
}

#[tokio::test]
async fn steers_and_notices_join_in_arrival_order() {
    let scope = TaskScope::root("test", |_| {});
    let env = Arc::new(NoticeEnvironment::default());
    let (_model, session) = asking(&scope, env.clone(), vec![text_response(&["thanks"])]).await;
    let mut events = session.subscribe(Some(0));
    session.send(submit("c1", "ask me")).await;
    let interaction = super::asked(&mut events).await;
    env.finish(shell_notice(1, "b1"));
    queued(&mut events, "b1").await;
    session.send(steer("c2", "also b")).await;
    env.finish(shell_notice(2, "b2"));
    queued(&mut events, "b2").await;
    session.send(super::answer("c3", interaction)).await;
    until_finished(&mut events, 1).await;
    let view = session.view().await.unwrap();
    assert_eq!(
        absorbed(&view.messages),
        ["notice:b1", "prompt:also b", "notice:b2"]
    );
    assert_eq!(env.acked(), [1, 2]);
    assert!(view.queue.is_empty());
}

#[tokio::test]
async fn notices_left_at_the_end_share_one_turn_before_queued_input() {
    let gate = Arc::new(Notify::new());
    let scope = TaskScope::root("test", |_| {});
    let env = Arc::new(NoticeEnvironment::default());
    // 第一次响应等放行（没有工具边界），之后按剧本。
    let model = Arc::new(GatedThenScripted {
        gate: gate.clone(),
        first: std::sync::atomic::AtomicBool::new(true),
        rest: ScriptedModel::new(vec![
            text_response(&["notices seen"]),
            text_response(&["queued seen"]),
        ]),
        requests: std::sync::Mutex::default(),
    });
    let session = session(&scope, env.clone(), model.clone(), Vec::new(), Vec::new()).await;
    let mut events = session.subscribe(Some(0));
    session.send(submit("c1", "first")).await;
    session.send(submit("c2", "queued later")).await;
    env.finish(shell_notice(1, "b1"));
    queued(&mut events, "b1").await;
    env.finish(shell_notice(2, "b2"));
    queued(&mut events, "b2").await;
    gate.notify_one();
    until_finished(&mut events, 3).await;
    // `next` 的通知先于 `later` 的排队输入；连续的通知合成一次模型请求。
    let requests = model.requests();
    assert_eq!(requests.len(), 3);
    let second = format!("{:?}", requests[1].messages);
    assert!(second.contains("b1") && second.contains("b2"), "{second}");
    assert!(!second.contains("queued later"));
    assert!(format!("{:?}", requests[2].messages.last()).contains("queued later"));
    let view = session.view().await.unwrap();
    assert_eq!(notices_in(&view.messages).len(), 2);
    assert_eq!(env.acked(), [1, 2]);
}

#[tokio::test]
async fn a_notice_already_absorbed_is_only_acked_when_it_comes_again() {
    let scope = TaskScope::root("test", |_| {});
    let env = Arc::new(NoticeEnvironment::default());
    let text = task_notification(&shell_notice(1, "b1"));
    let delivered = Message::Attachment(rust_cli_kernel_api::message::AttachmentMessage {
        id: rust_cli_kernel_api::ids::MessageId::new("msg_absorbed"),
        source: "queued_command".to_owned(),
        text: text.clone(),
        data: serde_json::json!({"prompt": text, "commandMode": "task-notification"}),
    });
    let model = Arc::new(ScriptedModel::new(Vec::new()));
    let session = session(
        &scope,
        env.clone(),
        model.clone(),
        Vec::new(),
        vec![delivered],
    )
    .await;
    env.finish(shell_notice(1, "b1"));
    tokio::time::timeout(Duration::from_secs(5), async {
        while env.acked().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(model.requests().is_empty());
    assert!(!session.view().await.unwrap().messages.iter().any(|message| {
        matches!(message, Message::User(user) if matches!(user.origin, UserOrigin::Meta { .. }))
    }));
}
