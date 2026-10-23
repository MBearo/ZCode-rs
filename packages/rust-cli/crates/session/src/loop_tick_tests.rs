//! `/loop` 哨兵的展开（tools-more.md §17.3）与会话关闭时停止持久调度（§16.4）。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::cron::{CronFire, CronSignals};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};
use rust_cli_kernel_api::session_api::{EndReason, SessionHandle};
use rust_cli_kernel_api::testing::{MemoryEnvironment, ScriptedModel, text_response};

use super::*;
use crate::test_support::{Setup, env, finished, until};

#[test]
fn autonomous_sentinels_send_the_preamble_once() {
    let mut ticks = LoopTicks::default();
    let first = ticks.autonomous(true);
    assert_eq!(
        first,
        format!("{}\n\n---\n\n{}", loops::preamble(), loops::tick_dynamic())
    );
    assert!(first.contains(".\n\n\n---\n\n# Autonomous loop tick (dynamic pacing)"));
    assert_eq!(ticks.autonomous(false), loops::tick_fixed());
    ticks = LoopTicks::default();
    assert!(
        ticks
            .autonomous(false)
            .starts_with("# Autonomous loop check")
    );
}

#[test]
fn loop_md_is_resent_only_when_it_changes() {
    let mut ticks = LoopTicks::default();
    let file = |content: &str| {
        Some(LoopFile {
            path: "/work/.zcode/loop.md".to_owned(),
            content: content.to_owned(),
        })
    };
    let first = ticks.file(true, file("- check CI"));
    assert!(first.starts_with("# /loop tick \u{2014} tasks from /work/.zcode/loop.md\n\n"));
    assert!(first.contains(
        "---\n\n- check CI\n\n---\n\n# /loop tick \u{2014} loop.md tasks (dynamic pacing)"
    ));
    assert_eq!(
        ticks.file(true, file("- check CI")),
        loops::file_tick_dynamic()
    );
    assert!(
        ticks
            .file(false, file("- check PRs"))
            .contains("- check PRs")
    );
    // loop.md 没了：退回自主说明，第一次带全文，之后只发“没有 loop.md”的短提醒……
    assert_eq!(
        ticks.file(true, None),
        with_preamble(loops::file_absent_dynamic())
    );
    assert_eq!(ticks.file(true, None), loops::file_absent_dynamic());
    // ……发过自主说明之后，自主哨兵也只发短提醒。
    assert_eq!(ticks.autonomous(true), loops::tick_dynamic());
}

/// 唤醒与到期任务由测试给出，记下 `session_closed`。
#[derive(Default)]
struct Cron {
    due: Mutex<VecDeque<CronFire>>,
    closed: Mutex<Vec<SessionId>>,
}

#[async_trait]
impl CronSignals for Cron {
    fn wakeups(&self, _session: &SessionId) -> BoxStream<'static, ()> {
        // 打开会话就唤醒一次：空闲的 actor 立即取到期任务。
        futures_util::stream::once(async {})
            .chain(futures_util::stream::pending())
            .boxed()
    }

    async fn take_due(&self, _session: &SessionId) -> Vec<CronFire> {
        let mut due = self.due.lock().unwrap_or_else(PoisonError::into_inner);
        due.pop_front().into_iter().collect()
    }

    async fn session_closed(&self, session: &SessionId) {
        self.closed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(session.clone());
    }
}

fn scheduled(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user)
                if matches!(&user.origin, UserOrigin::Meta { source } if source == "scheduled-task") =>
            {
                match user.content.as_slice() {
                    [ContentBlock::Text { text }] => Some(text.clone()),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect()
}

/// 触发的哨兵在开回合时展开：loop.md 从工作区的 `.zcode/loop.md` 读；关闭会话时通知任务表。
#[tokio::test]
async fn a_fired_sentinel_becomes_the_loop_md_tasks() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["done"])]));
    let cron = Arc::new(Cron::default());
    cron.due
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push_back(CronFire {
            job: "1a2b3c4d".to_owned(),
            prompt: "<<loop.md>>".to_owned(),
            kind: rust_cli_kernel_api::cron::CronFireKind::Loop,
        });
    let environment: Arc<MemoryEnvironment> = Arc::new(
        Arc::try_unwrap(env())
            .unwrap_or_else(|_| unreachable!("fresh environment"))
            .with_file("/work/.zcode/loop.md", "\n- run the tests\n"),
    );
    let mut setup = Setup::new(model);
    setup.cron = cron.clone();
    setup.env = environment;
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    let prompts = scheduled(&view.messages);
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].starts_with("# /loop tick \u{2014} tasks from /work/.zcode/loop.md"));
    assert!(prompts[0].contains("\n- run the tests\n"));
    assert!(prompts[0].ends_with(loops::file_tick_fixed()));
    built.session.close(EndReason::Clear).await;
    assert_eq!(
        *cron.closed.lock().unwrap_or_else(PoisonError::into_inner),
        [SessionId::new("sess_1")]
    );
}
