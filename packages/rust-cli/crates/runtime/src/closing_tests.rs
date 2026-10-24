//! 会话表的关闭中与再打开（docs/specs/rust-cli/session-lifecycle.md §3.3.1，修复线 F33）。
//!
//! 关闭停在执行环境的 `close_session` 上（testing 的 [`ClosingEnvironment`]），冷加载停在 [`HeldLoads`] 上：测试在
//! 停住的时候做要验证的操作，再放行，竞态因此确定地发生，不靠睡眠。

use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use rust_cli_hooks::{HookSource, HooksConfig, HooksSettings};
use rust_cli_kernel_api::env::{EnvPath, FileVersion, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId};
use rust_cli_kernel_api::session_api::{
    Command, CommandEnvelope, CreateSession, Delivery, EndReason, KernelEvent, StartSource,
};
use rust_cli_kernel_api::store::{
    CommitReceipt, FactBatch, LoadedSession, SessionFilter, SessionStore, SessionSummary,
};
use rust_cli_kernel_api::testing::{
    ClosingEnvironment, FixedClock, MemoryStore, ScriptedModel, SequentialIds, environments,
    selection, text_response,
};
use rust_cli_tools::file_state::{Baseline, FileState};
use tokio::sync::watch;

use super::*;

const WAIT: Duration = Duration::from_secs(5);

fn kernel(
    env: Arc<ClosingEnvironment>,
    store: Arc<dyn SessionStore>,
    model: Arc<ScriptedModel>,
) -> LocalKernel {
    let kernel = LocalKernel::new(
        Ports {
            model,
            environments: environments(env),
            store,
            artifacts: Arc::new(MemoryStore::default()),
            clock: Arc::new(FixedClock(1_000)),
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
    );
    // SessionStart 与 SessionEnd 各一个 command hook：执行环境记下它们的 stdin，看得出谁在什么时候跑过。
    let settings = serde_json::json!({"hooks": {
        "SessionStart": [{"hooks": [{"type": "command", "command": "start"}]}],
        "SessionEnd": [{"hooks": [{"type": "command", "command": "end"}]}],
    }});
    let (parsed, warnings) = HooksSettings::from_json(&settings);
    assert!(warnings.is_empty(), "{warnings:?}");
    kernel.replace_hooks(HooksConfig {
        version: 1,
        sources: vec![(HookSource::User, parsed)],
        ..HooksConfig::default()
    });
    kernel
}

fn request() -> CreateSession {
    CreateSession {
        id: None,
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/work"),
        },
        selection: selection(),
        mode: PermissionMode::Default,
        plan: false,
    }
}

/// 跑过的会话 hook，按顺序：`SessionStart:startup`、`SessionEnd:clear` 这样。
fn hooks_run(env: &ClosingEnvironment) -> Vec<String> {
    env.hook_inputs()
        .iter()
        .map(|input| {
            let input: serde_json::Value = serde_json::from_str(input).unwrap();
            let detail = input
                .get("source")
                .or_else(|| input.get("reason"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            format!("{}:{detail}", input["hook_event_name"].as_str().unwrap())
        })
        .collect()
}

/// 发一条输入，等它的回合结束：SessionStart 是 actor 启动时的后台工作，期间的输入排队，回合结束说明它已跑完。
async fn run_a_turn(session: &Arc<dyn SessionHandle>, text: &str) {
    let mut events = session.subscribe(Some(0));
    session
        .send(CommandEnvelope {
            id: CommandId::new(text),
            command: Command::Submit {
                text: text.to_owned(),
                attachments: Vec::new(),
                delivery: Delivery::Queue,
            },
        })
        .await;
    loop {
        let event = tokio::time::timeout(WAIT, events.next())
            .await
            .expect("the turn must finish")
            .unwrap();
        if matches!(event.event, KernelEvent::TurnFinished { .. }) {
            return;
        }
    }
}

/// 复现（F32 记下的竞态）：关闭停在 `close_session` 时（actor 已退出、还没移出会话表），以前打开同一个会话直接拿到
/// 旧句柄，`view()` 报 `the session is closed`。现在打开等关闭走完：放行之前不返回，之后是新 actor，按会话的表已经
/// 释放（readFileState 里旧会话的记录不在了），SessionStart(resume) 照常跑。
#[tokio::test]
async fn opening_a_closing_session_waits_for_the_close_and_starts_a_new_actor() {
    let env = Arc::new(ClosingEnvironment::held());
    let model = Arc::new(ScriptedModel::new(vec![
        text_response(&["first"]),
        text_response(&["again"]),
    ]));
    let kernel = kernel(env.clone(), Arc::new(MemoryStore::default()), model);
    let id = kernel.create_session(request()).await.unwrap();
    let first = kernel.open_session(&id).await.unwrap();
    // 先让 SessionStart(startup) 跑完，hook 的顺序才确定。
    run_a_turn(&first, "hello").await;
    kernel.closer.read_files.set(
        &id,
        "/work/a.txt",
        FileState {
            content: "a".to_owned(),
            baseline: Baseline::Version(FileVersion::new("v1")),
            offset: None,
            limit: None,
            partial: false,
            not_in_context: false,
        },
    );

    let close = first.close(EndReason::Clear);
    let reopen = async {
        env.closing(1).await;
        let mut open = pin!(kernel.open_session_as(&id, StartSource::Resume));
        assert!(
            futures_util::poll!(open.as_mut()).is_pending(),
            "the open must wait while the session is closing"
        );
        env.release();
        let reopened = open.await.unwrap();
        // 按会话的表在条目移出之前就清掉了：醒来的打开看到的是释放干净的状态。
        assert!(
            kernel.closer.read_files.get(&id, "/work/a.txt").is_none(),
            "the old session's readFileState must be released before the reopen"
        );
        reopened
    };
    let (_, reopened) = tokio::time::timeout(WAIT, async { tokio::join!(close, reopen) })
        .await
        .expect("the close and the reopen must finish");

    assert!(!Arc::ptr_eq(&first, &reopened), "a new actor");
    assert!(first.view().await.is_err(), "the old actor has stopped");
    run_a_turn(&reopened, "again").await;
    assert_eq!(
        hooks_run(&env),
        [
            "SessionStart:startup",
            "SessionEnd:clear",
            "SessionStart:resume"
        ]
    );
}

/// 关闭从第一次轮询起就是 Closing（早于 SessionEnd，也不等关闭任务被调度）：这时打开同一个会话同样要等。以前这段
/// 时间打开拿到的是马上要退出的 actor。
#[tokio::test]
async fn a_session_is_closing_from_the_first_poll_of_close() {
    let env = Arc::new(ClosingEnvironment::held());
    let kernel = kernel(
        env.clone(),
        Arc::new(MemoryStore::default()),
        Arc::new(ScriptedModel::new(Vec::new())),
    );
    let id = kernel.create_session(request()).await.unwrap();
    let first = kernel.open_session(&id).await.unwrap();
    let mut close = pin!(first.close(EndReason::Clear));
    assert!(futures_util::poll!(close.as_mut()).is_pending());
    let mut open = pin!(kernel.open_session_as(&id, StartSource::Resume));
    assert!(
        futures_util::poll!(open.as_mut()).is_pending(),
        "the open must wait from the first poll of close, before SessionEnd"
    );
    env.release();
    let (_, reopened) = tokio::time::timeout(WAIT, async { tokio::join!(close, open) })
        .await
        .expect("the close and the reopen must finish");
    assert!(!Arc::ptr_eq(&first, &reopened.unwrap()));
}

/// 两个打开都在等同一次关闭：放行后它们拿到同一个新 actor（以前两个都拿到旧句柄）。
#[tokio::test]
async fn two_opens_waiting_on_a_close_get_the_same_new_actor() {
    let env = Arc::new(ClosingEnvironment::held());
    let kernel = kernel(
        env.clone(),
        Arc::new(MemoryStore::default()),
        Arc::new(ScriptedModel::new(Vec::new())),
    );
    let id = kernel.create_session(request()).await.unwrap();
    let first = kernel.open_session(&id).await.unwrap();
    let close = first.close(EndReason::Resume);
    let opens = async {
        env.closing(1).await;
        let mut one = pin!(kernel.open_session_as(&id, StartSource::Resume));
        let mut two = pin!(kernel.open_session_as(&id, StartSource::Resume));
        assert!(futures_util::poll!(one.as_mut()).is_pending());
        assert!(futures_util::poll!(two.as_mut()).is_pending());
        env.release();
        let (one, two) = tokio::join!(one, two);
        (one.unwrap(), two.unwrap())
    };
    let (_, (one, two)) = tokio::time::timeout(WAIT, async { tokio::join!(close, opens) })
        .await
        .expect("the close and the opens must finish");
    assert!(Arc::ptr_eq(&one, &two), "both opens get the same actor");
    assert!(!Arc::ptr_eq(&one, &first), "not the closed one");
    one.view().await.unwrap();
}

/// 等待中的打开被丢下（例如 TUI 退出时丢掉在途的操作）：只丢掉它自己，关闭照常走完，之后的打开照常。
#[tokio::test]
async fn an_open_dropped_while_waiting_leaves_the_close_alone() {
    let env = Arc::new(ClosingEnvironment::held());
    let kernel = kernel(
        env.clone(),
        Arc::new(MemoryStore::default()),
        Arc::new(ScriptedModel::new(Vec::new())),
    );
    let id = kernel.create_session(request()).await.unwrap();
    let first = kernel.open_session(&id).await.unwrap();
    let close = first.close(EndReason::Clear);
    let dropped = async {
        env.closing(1).await;
        let mut open = pin!(kernel.open_session_as(&id, StartSource::Resume));
        assert!(futures_util::poll!(open.as_mut()).is_pending());
        // 离开这个块时丢下等待中的打开。
    };
    tokio::time::timeout(WAIT, async {
        tokio::join!(close, async {
            dropped.await;
            env.release();
        })
    })
    .await
    .expect("the close must finish");
    assert!(
        !kernel.sessions().contains_key(&id),
        "the close finished and the session left the kernel"
    );
    let again = kernel.open_session(&id).await.unwrap();
    assert!(!Arc::ptr_eq(&first, &again));
    again.view().await.unwrap();
}

/// 关闭失败（执行环境的 `close_session` 出错）也照常走完释放：等它的打开醒来，冷加载出新 actor，不会一直等。
#[tokio::test]
async fn a_failed_close_still_releases_the_session() {
    let env = Arc::new(ClosingEnvironment::held());
    let kernel = kernel(
        env.clone(),
        Arc::new(MemoryStore::default()),
        Arc::new(ScriptedModel::new(Vec::new())),
    );
    let id = kernel.create_session(request()).await.unwrap();
    let first = kernel.open_session(&id).await.unwrap();
    let close = first.close(EndReason::Resume);
    let reopen = async {
        env.closing(1).await;
        let mut open = pin!(kernel.open_session_as(&id, StartSource::Resume));
        assert!(futures_util::poll!(open.as_mut()).is_pending());
        env.fail();
        open.await.unwrap()
    };
    let (_, reopened) = tokio::time::timeout(WAIT, async { tokio::join!(close, reopen) })
        .await
        .expect("a failed close must still release the session");
    assert!(!Arc::ptr_eq(&first, &reopened));
    reopened.view().await.unwrap();
}

/// 关闭中又来关闭（同一个句柄）：只关一次——SessionEnd 只跑一次，提示归第一次的调用方，第二次等它关完才返回。
/// 以前 actor 还在时第二次 `close` 再跑一次 SessionEnd。
#[tokio::test]
async fn closing_twice_runs_session_end_once() {
    let env = Arc::new(ClosingEnvironment::held());
    let kernel = kernel(
        env.clone(),
        Arc::new(MemoryStore::default()),
        Arc::new(ScriptedModel::new(Vec::new())),
    );
    let id = kernel.create_session(request()).await.unwrap();
    let first = kernel.open_session(&id).await.unwrap();
    let (one, two, ()) = tokio::time::timeout(WAIT, async {
        tokio::join!(
            first.close(EndReason::Clear),
            first.close(EndReason::Clear),
            async {
                env.closing(1).await;
                env.release();
            }
        )
    })
    .await
    .expect("both closes must finish");
    assert_eq!(
        hooks_run(&env)
            .iter()
            .filter(|hook| hook.starts_with("SessionEnd"))
            .count(),
        1,
        "{:?}",
        hooks_run(&env)
    );
    assert_eq!(one.len(), 1, "the failed SessionEnd hook: {one:?}");
    assert!(two.is_empty(), "{two:?}");
    assert_eq!(env.close_calls(), 1, "the session is closed once");
    assert!(
        first.close(EndReason::Clear).await.is_empty(),
        "already closed"
    );
    assert!(!kernel.sessions().contains_key(&id));
}

/// 关闭的调用方走了（future 被丢下）：关闭在内核的任务里照样走完，打开不会一直等。以前关闭在调用方的任务里跑，
/// 丢下之后会话留在表里（actor 已退出），打开拿到旧句柄。
#[tokio::test]
async fn a_close_whose_caller_goes_away_still_finishes() {
    let env = Arc::new(ClosingEnvironment::held());
    let kernel = kernel(
        env.clone(),
        Arc::new(MemoryStore::default()),
        Arc::new(ScriptedModel::new(Vec::new())),
    );
    let id = kernel.create_session(request()).await.unwrap();
    let first = kernel.open_session(&id).await.unwrap();
    {
        let mut close = pin!(first.close(EndReason::Clear));
        tokio::select! {
            _ = close.as_mut() => panic!("the close cannot finish while the environment holds it"),
            () = env.closing(1) => {}
        }
    }
    env.release();
    let reopened = tokio::time::timeout(WAIT, kernel.open_session_as(&id, StartSource::Resume))
        .await
        .expect("the close must finish without its caller")
        .unwrap();
    assert!(!Arc::ptr_eq(&first, &reopened));
    reopened.view().await.unwrap();
}

/// 冷加载停在闸门上的存储：数 `load` 的次数，放行后转给内存存储；`fail_next` 让下一个通过的加载失败。
struct HeldLoads {
    inner: Arc<MemoryStore>,
    gate: watch::Sender<Gate>,
}

#[derive(Default)]
struct Gate {
    loads: usize,
    open: bool,
    failures: usize,
}

impl HeldLoads {
    fn held(inner: Arc<MemoryStore>) -> Self {
        Self {
            inner,
            gate: watch::Sender::new(Gate::default()),
        }
    }

    async fn loading(&self, count: usize) {
        let _ = self
            .gate
            .subscribe()
            .wait_for(|gate| gate.loads >= count)
            .await;
    }

    fn loads(&self) -> usize {
        self.gate.borrow().loads
    }

    fn release(&self) {
        self.gate.send_modify(|gate| gate.open = true);
    }

    fn fail_next(&self) {
        self.gate.send_modify(|gate| gate.failures += 1);
    }
}

#[async_trait]
impl SessionStore for HeldLoads {
    async fn create(&self, meta: SessionMeta) -> Result<(), StoreError> {
        self.inner.create(meta).await
    }

    async fn commit(
        &self,
        session: &SessionId,
        batch: FactBatch,
    ) -> Result<CommitReceipt, StoreError> {
        self.inner.commit(session, batch).await
    }

    async fn load(&self, session: &SessionId) -> Result<LoadedSession, StoreError> {
        self.gate.send_modify(|gate| gate.loads += 1);
        let _ = self.gate.subscribe().wait_for(|gate| gate.open).await;
        let mut failed = false;
        self.gate.send_modify(|gate| {
            if gate.failures > 0 {
                gate.failures -= 1;
                failed = true;
            }
        });
        if failed {
            return Err(StoreError::Storage {
                message: "the test store failed to load".to_owned(),
            });
        }
        self.inner.load(session).await
    }

    async fn list(&self, filter: SessionFilter) -> Result<Vec<SessionSummary>, StoreError> {
        self.inner.list(filter).await
    }
}

/// 存储里有一个会话、内核里没有它（另一个进程写的，或上次运行留下的）。
async fn stored_session(store: &Arc<MemoryStore>) -> SessionId {
    let writer = kernel(
        Arc::new(ClosingEnvironment::held()),
        store.clone(),
        Arc::new(ScriptedModel::new(Vec::new())),
    );
    writer.create_session(request()).await.unwrap()
}

/// 并发两次打开同一个会话：只冷加载一次（另一个等它），拿到同一个句柄。以前两个各自冷加载，后到的那份丢掉。
#[tokio::test]
async fn two_opens_of_one_session_share_one_cold_load() {
    let memory = Arc::new(MemoryStore::default());
    let id = stored_session(&memory).await;
    let store = Arc::new(HeldLoads::held(memory));
    let kernel = kernel(
        Arc::new(ClosingEnvironment::held()),
        store.clone(),
        Arc::new(ScriptedModel::new(Vec::new())),
    );
    let (one, two, ()) = tokio::time::timeout(WAIT, async {
        tokio::join!(
            kernel.open_session_as(&id, StartSource::Resume),
            kernel.open_session_as(&id, StartSource::Resume),
            async {
                store.loading(1).await;
                store.release();
            }
        )
    })
    .await
    .expect("both opens must finish");
    let (one, two) = (one.unwrap(), two.unwrap());
    assert!(Arc::ptr_eq(&one, &two));
    assert_eq!(store.loads(), 1, "one cold load for both opens");
}

/// 冷加载失败：这个调用得到自己的错误，等它的调用醒来自己再加载。
#[tokio::test]
async fn a_failed_cold_load_lets_the_waiting_open_load_again() {
    let memory = Arc::new(MemoryStore::default());
    let id = stored_session(&memory).await;
    let store = Arc::new(HeldLoads::held(memory));
    let kernel = kernel(
        Arc::new(ClosingEnvironment::held()),
        store.clone(),
        Arc::new(ScriptedModel::new(Vec::new())),
    );
    let (one, two, ()) = tokio::time::timeout(WAIT, async {
        tokio::join!(
            kernel.open_session_as(&id, StartSource::Resume),
            kernel.open_session_as(&id, StartSource::Resume),
            async {
                store.loading(1).await;
                store.fail_next();
                store.release();
            }
        )
    })
    .await
    .expect("both opens must finish");
    assert!(one.is_err(), "the first load failed");
    two.unwrap().view().await.unwrap();
    assert_eq!(store.loads(), 2);
}

/// 冷加载的调用被丢下：“加载中”随之移出，等它的调用醒来接着加载。
#[tokio::test]
async fn dropping_the_loading_open_hands_the_load_to_the_waiting_one() {
    let memory = Arc::new(MemoryStore::default());
    let id = stored_session(&memory).await;
    let store = Arc::new(HeldLoads::held(memory));
    let kernel = kernel(
        Arc::new(ClosingEnvironment::held()),
        store.clone(),
        Arc::new(ScriptedModel::new(Vec::new())),
    );
    let mut waiting = pin!(kernel.open_session_as(&id, StartSource::Resume));
    {
        let mut loading = pin!(kernel.open_session_as(&id, StartSource::Resume));
        assert!(futures_util::poll!(loading.as_mut()).is_pending());
        assert!(futures_util::poll!(waiting.as_mut()).is_pending());
    }
    store.release();
    let opened = tokio::time::timeout(WAIT, waiting)
        .await
        .expect("the waiting open must take over the load")
        .unwrap();
    opened.view().await.unwrap();
    assert_eq!(store.loads(), 2);
}
