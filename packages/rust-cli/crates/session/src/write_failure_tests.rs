//! 回合里的提交失败（docs/specs/rust-cli/usage.md §2.5、logging.md §4，工作线 F77）：回合以存储错误结束，同时记
//! 一条带原因的 warn。以前错误只进回合的结局，日志里没有一行。
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::{Arc, Mutex};

use rust_cli_kernel_api::session_api::{KernelEvent, SessionHandle, TurnOutcome};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use tracing::field::Field;
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

use crate::test_support::{Setup, finished, submit, until};

/// 当前线程上的 warn 事件，每条是字段名 → 值（`Debug` 的文字）。
#[derive(Clone, Default)]
struct Warnings(Arc<Mutex<Vec<BTreeMap<&'static str, String>>>>);

impl Subscriber for Warnings {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        if *event.metadata().level() != Level::WARN {
            return;
        }
        let mut fields = BTreeMap::new();
        event.record(&mut |field: &Field, value: &dyn Debug| {
            fields.insert(field.name(), format!("{value:?}"));
        });
        self.0.lock().unwrap().push(fields);
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

#[tokio::test]
async fn a_failed_write_ends_the_turn_and_is_logged() {
    // 单线程运行时：actor 与回合任务都在这个线程上，订阅者看得到它们的事件。
    let warnings = Warnings::default();
    let _guard = tracing::subscriber::set_default(warnings.clone());
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(vec![text_response(&["hi"])])));
    // 存储里没有这个会话：每次提交都失败。
    setup.stored = false;
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    built.session.send(submit("c1", "hello")).await;
    let seen = until(&mut events, finished).await;
    let Some(KernelEvent::TurnFinished {
        outcome: TurnOutcome::Failed { message },
        ..
    }) = seen.last()
    else {
        panic!("{seen:?}");
    };
    let logged: Vec<_> = warnings
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|fields| {
            fields.get("event").map(String::as_str) == Some("\"session.write.failed\"")
        })
        .cloned()
        .collect();
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0]["session"], "sess_1");
    assert_eq!(&logged[0]["reason"], message);
}
