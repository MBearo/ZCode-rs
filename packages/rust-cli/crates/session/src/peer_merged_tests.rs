//! 重开会话时已经并入的同伴消息与回执不再投递（docs/specs/rust-cli/cross-session.md §2.2、§2.4、§3.3，F139）：并入与
//! `peer_merged` 是同一批；压缩之后按 node-store 的形状冷加载（压缩点之前的只供显示），按事实判断；旧会话的 entry 在
//! 完整历史里按文字找一次并补记；并入的那一批失败时两者都不在库里。端口是测试替身（peers_tests.rs）。

use rust_cli_base::sync::LockUnpoisoned;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rust_cli_kernel_api::ids::{MessageId, OwnerEpoch, SessionId};
use rust_cli_kernel_api::message::{
    AssistantMessage, CompactBoundary, CompactTrigger, ContentBlock, Message, StopReason,
    SystemMessage, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::peers::{
    ModeClass, PeerAddress, PeerReceipt, PeerSignal, ReceiptStatus, envelope,
};
use rust_cli_kernel_api::session_api::{EndReason, SessionHandle, SessionView};
use rust_cli_kernel_api::store::{Fact, FactBatch, SessionStore, StoreError, entry_kinds};
use rust_cli_kernel_api::testing::{
    FakeTool, MemoryStore, ScriptedModel, StoreCall, StoreHooks, TokioClock, text_response,
    tool_use_response,
};
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::Opened;
use crate::peers::tests::{FakePort, SENDER, deps, message, user_texts};
use crate::test_support::{Built, Setup, finished, meta, submit, until};

/// `message()` 的正文。
const BODY: &str = "hello from B";
/// 回执全文的开头。
const NOTICE: &str = "[Cross-session delivery notice]";
const HOLD_MS: u64 = 300_000;
/// 收到回执的一方记下的对方地址。
const RECIPIENT: &str = "/peers/1.sock";

/// 记下会话 actor 提交的每一批；`refuse` 打开时拒绝带 [`BODY`] 的消息的那一批（并入时存储失败）。
#[derive(Default)]
struct Batches {
    seen: Mutex<Vec<Vec<Fact>>>,
    refused: Mutex<Vec<Vec<Fact>>>,
    refuse: AtomicBool,
}

impl Batches {
    fn seen(&self) -> Vec<Vec<Fact>> {
        self.seen.lock_unpoisoned().clone()
    }

    fn refused(&self) -> Vec<Vec<Fact>> {
        self.refused.lock_unpoisoned().clone()
    }
}

#[async_trait]
impl StoreHooks for Batches {
    async fn before(&self, call: StoreCall<'_>) -> Result<(), StoreError> {
        let StoreCall::Commit(_, batch) = call else {
            return Ok(());
        };
        if self.refuse.load(Ordering::SeqCst) && carries(&batch.facts, BODY) {
            self.refused.lock_unpoisoned().push(batch.facts.clone());
            return Err(StoreError::Storage {
                message: "refused by the test".to_owned(),
            });
        }
        self.seen.lock_unpoisoned().push(batch.facts.clone());
        Ok(())
    }
}

fn session() -> SessionId {
    meta().id
}

fn text_of(message: &Message) -> String {
    match message {
        Message::User(user) => user
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect(),
        Message::Attachment(attachment) => attachment.text.clone(),
        _ => String::new(),
    }
}

/// 这一批里有一条文字含 `needle` 的消息（entry 的 data 不算）。
fn carries(facts: &[Fact], needle: &str) -> bool {
    facts.iter().any(
        |fact| matches!(fact, Fact::MessageAppended { message } if text_of(message).contains(needle)),
    )
}

/// 这一批里 `peer_merged` 的 id，按顺序。
fn merged_ids(facts: &[Fact]) -> Vec<String> {
    facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::Entry { kind, id, .. } if kind == entry_kinds::PEER_MERGED => Some(id.clone()),
            _ => None,
        })
        .collect()
}

/// 带 `needle` 的消息所在的那一批。
fn batch_with(batches: &[Vec<Fact>], needle: &str) -> Vec<Fact> {
    batches
        .iter()
        .find(|facts| carries(facts, needle))
        .cloned()
        .unwrap_or_else(|| panic!("no batch carries {needle:?}"))
}

fn count(messages: &[Message], needle: &str) -> usize {
    messages
        .iter()
        .filter(|message| text_of(message).contains(needle))
        .count()
}

fn sent(model: &ScriptedModel, needle: &str) -> bool {
    model
        .requests()
        .iter()
        .any(|request| format!("{:?}", request.messages).contains(needle))
}

async fn stored() -> Arc<MemoryStore> {
    let store = Arc::new(MemoryStore::default());
    store.create(meta()).await.unwrap();
    store
}

async fn seed(store: &MemoryStore, facts: Vec<Fact>) {
    let batch = FactBatch {
        owner_epoch: OwnerEpoch::LOCAL,
        command: None,
        facts,
        turn: None,
    };
    store.commit(&session(), batch).await.unwrap();
}

fn appended(message: Message) -> Fact {
    Fact::MessageAppended { message }
}

fn user(id: &str, text: &str, origin: UserOrigin) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text { text: text.into() }],
        origin,
    })
}

fn reply(id: &str, text: &str) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text { text: text.into() }],
        model: "m".into(),
        stop_reason: Some(StopReason::EndTurn),
        usage: None,
    })
}

/// 会话关闭之后写入一个压缩点（边界与摘要）；之后的冷加载按 node-store 的形状切分。
async fn compact(store: &MemoryStore) {
    let summary = MessageId::new("msg_summary");
    let boundary = Message::System(SystemMessage::CompactBoundary(CompactBoundary {
        id: MessageId::new("msg_boundary"),
        trigger: CompactTrigger::Auto,
        pre_tokens: 100,
        summary: summary.clone(),
        preserved: None,
        discovered_tools: Vec::new(),
    }));
    let summary = user(summary.as_str(), "SUMMARY", UserOrigin::CompactSummary);
    seed(store, vec![appended(boundary), appended(summary)]).await;
    store.split_at_compaction();
}

async fn close(built: Built) {
    built.session.close(EndReason::Other).await;
}

/// 同一个存储冷加载再打开（新的入站通道）。
async fn reopen(
    store: &Arc<MemoryStore>,
    port: &Arc<FakePort>,
    model: Arc<ScriptedModel>,
) -> Built {
    port.reset();
    let loaded = store.load(&session()).await.unwrap();
    let mut setup = Setup::new(model);
    setup.peers = deps(port, HOLD_MS, true);
    setup.store = Some(store.clone());
    setup.messages = loaded.messages;
    setup.earlier = loaded.earlier;
    setup.opened = Opened::Loaded;
    setup.build().await
}

/// 重开之后的模型：够旧代码为重投的消息多开的回合用。
fn later_model() -> Arc<ScriptedModel> {
    Arc::new(ScriptedModel::new(vec![
        text_response(&["first"]),
        text_response(&["second"]),
        text_response(&["third"]),
    ]))
}

/// 重开之后提交一条输入，等到它答完，而且没有别的回合在跑或排队。
async fn answer_after_reopen(built: &Built, text: &str) -> SessionView {
    let mut events = built.session.subscribe(None);
    built.session.send(submit("c_after", text)).await;
    loop {
        until(&mut events, finished).await;
        let view = built.session.view().await.unwrap();
        let answered = user_texts(&view.messages)
            .iter()
            .any(|(_, said)| said == text);
        if answered && !view.running && view.queue.is_empty() {
            return view;
        }
    }
}

/// 等到 `done` 成立（5 s 内，只用于测试判定）。
async fn wait_for(what: &str, done: impl Fn() -> bool) {
    for _ in 0..500 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

fn store_merged(store: &MemoryStore) -> Vec<String> {
    merged_ids(&store.facts(&session()))
}

#[tokio::test]
async fn a_message_merged_on_an_idle_turn_is_not_delivered_again_after_compaction() {
    let port = FakePort::new();
    let store = stored().await;
    let batches = Arc::new(Batches::default());
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(vec![text_response(&["ok"])])));
    setup.peers = deps(&port, HOLD_MS, true);
    setup.store = Some(store.clone());
    setup.store_hooks = Some(batches.clone());
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    port.push(message(1, "m1", ModeClass::Prompting, 0));
    until(&mut events, finished).await;
    // 开回合的 meta user 消息与它并入的事实是同一批。
    assert_eq!(
        merged_ids(&batch_with(&batches.seen(), BODY)),
        ["message:m1"]
    );
    close(built).await;

    compact(&store).await;
    let model = later_model();
    let built = reopen(&store, &port, model.clone()).await;
    let view = answer_after_reopen(&built, "after reopen").await;
    // 屏幕上（完整历史）只有一份；重开之后的请求里没有它。
    assert_eq!(count(&view.messages, BODY), 1);
    assert!(!sent(&model, BODY));
    assert_eq!(store_merged(&store), ["message:m1"]);
}

#[tokio::test]
async fn a_message_merged_at_a_tool_boundary_is_not_delivered_again_after_compaction() {
    let port = FakePort::new();
    let store = stored().await;
    let batches = Arc::new(Batches::default());
    let gate = Arc::new(Notify::new());
    let tool = FakeTool::new("Wait", true, "waited").gated(gate.clone());
    let started = tool.started.clone();
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Wait", &json!({})),
        text_response(&["ok"]),
    ]));
    let mut setup = Setup::new(model.clone());
    setup.tools = vec![Arc::new(tool)];
    setup.peers = deps(&port, HOLD_MS, true);
    setup.store = Some(store.clone());
    setup.store_hooks = Some(batches.clone());
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    built.session.send(submit("c1", "wait")).await;
    started.notified().await;
    port.push(message(1, "m1", ModeClass::Prompting, 0));
    // 提交并 ack 之后它在通道里，工具结束时在这个工具边界并入。
    wait_for("the ack", || port.acked.lock_unpoisoned().contains(&1)).await;
    gate.notify_one();
    until(&mut events, finished).await;
    assert!(format!("{:?}", model.requests()[1].messages).contains("while you were working"));
    let batch = batch_with(&batches.seen(), BODY);
    assert!(batch.iter().any(|fact| matches!(
        fact,
        Fact::MessageAppended {
            message: Message::Attachment(_)
        }
    )));
    assert_eq!(merged_ids(&batch), ["message:m1"]);
    close(built).await;

    compact(&store).await;
    let model = later_model();
    let built = reopen(&store, &port, model.clone()).await;
    let view = answer_after_reopen(&built, "after reopen").await;
    assert_eq!(count(&view.messages, BODY), 1);
    assert!(!sent(&model, BODY));
}

fn receipt(seq: u64, msg_id: &str) -> PeerSignal {
    PeerSignal::Receipt(PeerReceipt {
        seq,
        msg_id: msg_id.to_owned(),
        status: ReceiptStatus::Held,
        recipient: PeerAddress::from_socket(RECIPIENT),
    })
}

#[tokio::test]
async fn merged_receipts_are_not_delivered_again_after_compaction() {
    let port = FakePort::new();
    let store = stored().await;
    let batches = Arc::new(Batches::default());
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(vec![text_response(&["ok"])])));
    setup.peers = deps(&port, HOLD_MS, true);
    setup.store = Some(store.clone());
    setup.store_hooks = Some(batches.clone());
    // 合并窗口按时钟判断：时钟跟着 tokio 时间走。
    setup.clock = Arc::new(TokioClock::new(1_000));
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    port.push(receipt(1, "m8"));
    port.push(receipt(2, "m9"));
    until(&mut events, finished).await;
    // 两条合成一条消息；每条回执各一条事实，与那条消息同一批。
    let batch = batch_with(&batches.seen(), NOTICE);
    assert!(carries(&batch, "2 of your messages"));
    assert_eq!(merged_ids(&batch), ["receipt:m8:held", "receipt:m9:held"]);
    close(built).await;

    compact(&store).await;
    let model = later_model();
    let built = reopen(&store, &port, model.clone()).await;
    let view = answer_after_reopen(&built, "after reopen").await;
    assert_eq!(count(&view.messages, NOTICE), 1);
    assert!(!sent(&model, NOTICE));
}

#[tokio::test]
async fn a_passive_receipt_merged_at_a_tool_boundary_records_its_fact() {
    // `-p` 的回执只在工具边界并入：收下之后还没到工具边界就退出了，重开后的下一个工具边界并入，事实与 attachment
    // 同一批；之后再重开不再投递。
    let store = stored().await;
    let recipient = PeerAddress::from_socket(RECIPIENT);
    seed(
        &store,
        vec![Fact::Entry {
            kind: entry_kinds::PEER_RECEIPT.to_owned(),
            id: "m9:held".to_owned(),
            data: json!({
                "msgId": "m9",
                "status": "held",
                "recipient": recipient.as_str(),
                "mergeTracked": true,
            }),
        }],
    )
    .await;
    let port = FakePort::new();
    let batches = Arc::new(Batches::default());
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Wait", &json!({})),
        text_response(&["ok"]),
    ]));
    let mut setup = Setup::new(model.clone());
    setup.tools = vec![Arc::new(FakeTool::new("Wait", true, "waited"))];
    setup.peers = deps(&port, HOLD_MS, false);
    setup.store = Some(store.clone());
    setup.store_hooks = Some(batches.clone());
    setup.opened = Opened::Loaded;
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    built.session.send(submit("c1", "wait")).await;
    until(&mut events, finished).await;
    // 第一个请求里没有（不为它开回合），工具边界之后的请求里有。
    let requests = model.requests();
    assert!(!format!("{:?}", requests[0].messages).contains(NOTICE));
    assert!(format!("{:?}", requests[1].messages).contains(NOTICE));
    let batch = batch_with(&batches.seen(), NOTICE);
    assert!(batch.iter().any(|fact| matches!(
        fact,
        Fact::MessageAppended {
            message: Message::Attachment(_)
        }
    )));
    assert_eq!(merged_ids(&batch), ["receipt:m9:held"]);
    close(built).await;

    compact(&store).await;
    let model = later_model();
    let built = reopen(&store, &port, model.clone()).await;
    let view = answer_after_reopen(&built, "after reopen").await;
    assert_eq!(count(&view.messages, NOTICE), 1);
    assert!(!sent(&model, NOTICE));
}

/// F139 之前写的 `peer_message`：没有 `mergeTracked`。
fn legacy_message(msg_id: &str, body: &str) -> (Fact, String) {
    let from = PeerAddress::from_socket(SENDER);
    let envelope = envelope(&from, Some("work-b-7f"), ModeClass::Prompting, body);
    let data = json!({
        "msgId": msg_id,
        "from": from.as_str(),
        "fromName": "work-b-7f",
        "fromMode": "prompting",
        "envelope": envelope,
        "receivedAt": 0,
        "held": Value::Null,
    });
    let fact = Fact::Entry {
        kind: entry_kinds::PEER_MESSAGE.to_owned(),
        id: msg_id.to_owned(),
        data,
    };
    (fact, envelope)
}

fn peer_turn(id: &str, envelope: &str) -> Message {
    user(
        id,
        &rust_cli_prompts::peers::wrap(envelope, false),
        UserOrigin::Meta {
            source: crate::peers::PEER_SOURCE.to_owned(),
        },
    )
}

#[tokio::test]
async fn a_legacy_entry_merged_before_the_compaction_is_recorded_once_and_not_delivered() {
    let store = stored().await;
    // F139 之前的库：entry 没有 mergeTracked，也没有 peer_merged。m1 与回执的文字在压缩点之前；m2 的文字哪里都没有。
    let (m1, first) = legacy_message("m1", "first from B");
    let (m2, _) = legacy_message("m2", "second from B");
    let recipient = PeerAddress::from_socket(RECIPIENT);
    let receipt_entry = Fact::Entry {
        kind: entry_kinds::PEER_RECEIPT.to_owned(),
        id: "m9:held".to_owned(),
        data: json!({"msgId": "m9", "status": "held", "recipient": recipient.as_str()}),
    };
    let receipt_text = rust_cli_prompts::peers::non_user(
        &rust_cli_prompts::peers::receipt("held", &[recipient.as_str()], &["m9"]).unwrap(),
    );
    let meta_origin = UserOrigin::Meta {
        source: crate::peers::PEER_SOURCE.to_owned(),
    };
    seed(
        &store,
        vec![
            m1,
            appended(peer_turn("msg_l1", &first)),
            appended(reply("msg_l2", "seen")),
            receipt_entry,
            appended(user("msg_l3", &receipt_text, meta_origin)),
            appended(reply("msg_l4", "noted")),
            m2,
        ],
    )
    .await;
    compact(&store).await;

    let port = FakePort::new();
    let model = later_model();
    let built = reopen(&store, &port, model.clone()).await;
    let view = answer_after_reopen(&built, "after reopen").await;
    assert_eq!(count(&view.messages, "first from B"), 1);
    assert_eq!(count(&view.messages, NOTICE), 1);
    assert!(!sent(&model, "first from B") && !sent(&model, NOTICE));
    // 找不到文字的照常投递一次（并入时记下事实）。
    assert_eq!(count(&view.messages, "second from B"), 1);
    assert!(sent(&model, "second from B"));
    let mut merged = store_merged(&store);
    merged.sort();
    assert_eq!(merged, ["message:m1", "message:m2", "receipt:m9:held"]);
    close(built).await;

    // 再打开一次：都有事实了，不再补记，也不再投递。
    let model = later_model();
    let built = reopen(&store, &port, model.clone()).await;
    let view = answer_after_reopen(&built, "third open").await;
    assert_eq!(count(&view.messages, "second from B"), 1);
    // 只有这条输入的一个回合（m2 已在模型侧的历史里，照常发出）。
    assert_eq!(model.requests().len(), 1);
    assert!(!sent(&model, "first from B"));
    assert_eq!(store_merged(&store).len(), 3);
}

#[tokio::test]
async fn a_tracked_message_without_its_fact_is_delivered_even_when_the_same_text_was_merged() {
    // 同一个发送方两次发了同样的正文：m1 已并入（有事实），m2 收下了还没并入就退出了。新写的 entry 只看事实，
    // 不因为对话里有同样的文字就当成已并入。
    let store = stored().await;
    let from = PeerAddress::from_socket(SENDER);
    let text = envelope(&from, Some("work-b-7f"), ModeClass::Prompting, BODY);
    let entry = |msg_id: &str| Fact::Entry {
        kind: entry_kinds::PEER_MESSAGE.to_owned(),
        id: msg_id.to_owned(),
        data: json!({
            "msgId": msg_id,
            "from": from.as_str(),
            "fromName": "work-b-7f",
            "fromMode": "prompting",
            "envelope": text,
            "receivedAt": 0,
            "held": Value::Null,
            "mergeTracked": true,
        }),
    };
    let merged = Fact::Entry {
        kind: entry_kinds::PEER_MERGED.to_owned(),
        id: "message:m1".to_owned(),
        data: json!({"msgId": "m1"}),
    };
    seed(
        &store,
        vec![
            entry("m1"),
            appended(peer_turn("msg_t1", &text)),
            merged,
            appended(reply("msg_t2", "seen")),
            entry("m2"),
        ],
    )
    .await;
    let port = FakePort::new();
    let built = reopen(&store, &port, later_model()).await;
    let view = answer_after_reopen(&built, "after reopen").await;
    assert_eq!(count(&view.messages, BODY), 2);
    let mut merged = store_merged(&store);
    merged.sort();
    assert_eq!(merged, ["message:m1", "message:m2"]);
}

#[tokio::test]
async fn a_refused_merge_stores_neither_the_message_nor_its_fact() {
    let port = FakePort::new();
    let store = stored().await;
    let batches = Arc::new(Batches::default());
    batches.refuse.store(true, Ordering::SeqCst);
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(vec![text_response(&["ok"])])));
    setup.peers = deps(&port, HOLD_MS, true);
    setup.store = Some(store.clone());
    setup.store_hooks = Some(batches.clone());
    let built = setup.build().await;
    port.push(message(1, "m1", ModeClass::Prompting, 0));
    wait_for("the refused merge", || !batches.refused().is_empty()).await;
    // 被拒绝的那一批里两者都有；库里两者都没有（不会只留一半）。
    assert_eq!(merged_ids(&batches.refused()[0]), ["message:m1"]);
    assert!(!carries(&store.facts(&session()), BODY));
    assert!(store_merged(&store).is_empty());
    close(built).await;

    // 重开：没有事实，照常投递一次。
    let built = reopen(&store, &port, later_model()).await;
    let view = answer_after_reopen(&built, "after reopen").await;
    assert_eq!(count(&view.messages, BODY), 1);
    assert_eq!(store_merged(&store), ["message:m1"]);
}
