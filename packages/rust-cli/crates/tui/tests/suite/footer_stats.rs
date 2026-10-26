//! footer 右侧的请求统计（docs/specs/rust-cli/tui-display.md §2.6，修复线 F27，用户决定）：真实的内核组装（会话
//! actor、压缩模块与用量表的查询）。完成一次请求后显示 TPS、ctx、cache；`/resume` 之后命中率从用量行重建；压缩后
//! ctx 立即更新。替身模型不在模型卡片里：窗口 200,000、预留 20,000，有效窗口 180,000。
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::{ContentBlock, StopReason, Usage};
use rust_cli_kernel_api::metering::RequestStats;
use rust_cli_kernel_api::model::{BlockDelta, ModelEvent};
use rust_cli_kernel_api::store::{
    CommitReceipt, FactBatch, LoadedSession, SessionFilter, SessionMeta, SessionPage, SessionQuery,
    SessionStore, SessionSummary, StoreError,
};
use rust_cli_kernel_api::testing::MemoryStore;
use rust_cli_tui::ui::palette::{ColorLevel, Palette};
use tokio::sync::watch;

use super::support::{GatedModel, Item, Setup, finish, send, with_ui};

/// 测试拨动的内核时钟：请求的开始、首字与结束各读一次（usage.md §1）。
#[derive(Default)]
struct ManualClock(AtomicU64);

impl ManualClock {
    fn set(&self, ms: u64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl rust_cli_kernel_api::clock::Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }

    fn time_zone(&self) -> String {
        "UTC".to_owned()
    }
}

fn usage(input: u64, read: u64, output: u64) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: output,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: read,
    }
}

/// 回复的开头（到第一个文字增量为止，带换行：流式中显示到最后一个换行）。
fn head(text: &str, input: Usage) -> Vec<Item> {
    vec![
        Ok(ModelEvent::MessageStart {
            id: "msg_api".to_owned(),
            model: "scripted".to_owned(),
            usage: input,
        }),
        Ok(ModelEvent::BlockStart {
            index: 0,
            block: ContentBlock::Text {
                text: String::new(),
            },
        }),
        Ok(ModelEvent::BlockDelta {
            index: 0,
            delta: BlockDelta::Text(format!("{text}\n")),
        }),
    ]
}

/// 回复的结尾：合并好的累计用量（开头的输入、这里的输出，query.md §1.10）与停止。
fn tail(input: Usage, output: u64) -> Vec<Item> {
    vec![
        Ok(ModelEvent::BlockStop { index: 0 }),
        Ok(ModelEvent::MessageDelta {
            stop_reason: Some(StopReason::EndTurn),
            usage: Usage {
                output_tokens: output,
                ..input
            },
        }),
        Ok(ModelEvent::MessageStop),
    ]
}

fn reply(text: &str, input: Usage, output: u64) -> Vec<Item> {
    [head(text, input), tail(input, output)].concat()
}

fn idle(screen: &str, text: &str) -> bool {
    screen.contains(text) && !screen.contains("esc to interrupt")
}

/// 等模型收到第 `count` 个请求（请求开始的时刻已经读过时钟）。
async fn requested(model: &GatedModel, count: usize) {
    let wait = async {
        while model.requests().len() < count {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    if tokio::time::timeout(Duration::from_secs(10), wait)
        .await
        .is_err()
    {
        panic!("timed out waiting for model request {count}");
    }
}

/// 一次请求：开始 1000、首字 3000、结束 6000，输出 120 → 120 × 1000 / 3000 = 40 tok/s；输入 200、缓存读 800（Node
/// 的输入是 1000）→ 80%；上下文 1120 / 180,000 → ctx 1%。流式中三项都还没有（还没有完成的请求）。
#[tokio::test(flavor = "multi_thread")]
async fn a_finished_request_shows_tps_context_and_cache() {
    let (model, mut senders) = GatedModel::new(1);
    let clock = Arc::new(ManualClock::default());
    let setup = Setup {
        width: 120,
        clock: Some(clock.clone()),
        ..Setup::default()
    };
    let gated = model.clone();
    with_ui(model, setup, |mut ui| async move {
        clock.set(1_000);
        ui.submit("first");
        requested(&gated, 1).await;
        clock.set(3_000);
        send(&senders[0], head("streaming line", usage(200, 800, 0)));
        let screen = ui.wait_for("streaming line").await;
        assert!(
            !screen.contains("tok/s") && !screen.contains("cache"),
            "{screen}"
        );
        // 流结束（发送端释放）时读结束时刻。
        clock.set(6_000);
        finish(senders.remove(0), tail(usage(200, 800, 0), 120));
        let screen = ui
            .wait("the stats", |s| {
                idle(s, "streaming line") && s.contains("cache")
            })
            .await;
        assert!(
            screen.contains("scripted · 40 tok/s · ctx 1% · cache 80%"),
            "{screen}"
        );
    })
    .await;
}

/// `/resume` 页的会话列表在测试放行之前取不到，其余直接交给内存存储：让“页面框架已经画出、列表还没到”这一刻每次都
/// 出现（修复线 F31）。
struct HeldList {
    inner: MemoryStore,
    open: watch::Receiver<bool>,
}

#[async_trait::async_trait]
impl SessionStore for HeldList {
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
        self.inner.load(session).await
    }

    async fn list(&self, filter: SessionFilter) -> Result<Vec<SessionSummary>, StoreError> {
        self.inner.list(filter).await
    }

    async fn entries(
        &self,
        session: &SessionId,
        kind: &str,
    ) -> Result<Vec<(String, serde_json::Value)>, StoreError> {
        self.inner.entries(session, kind).await
    }

    async fn request_stats(&self, session: &SessionId) -> Result<RequestStats, StoreError> {
        self.inner.request_stats(session).await
    }

    async fn query_sessions(&self, query: SessionQuery) -> Result<SessionPage, StoreError> {
        let mut open = self.open.clone();
        // 放行端随测试结束释放时也不再等。
        let _ = open.wait_for(|open| *open).await;
        self.inner.query_sessions(query).await
    }
}

/// 命中率是本会话的累计（Node 的口径）：第一次 0 / 1000，第二次 800 / 1000 → 40%。`/clear` 之后没有；`/resume`
/// 切回这个会话时从用量行重建（与新会话同一个查询），上下文用量来自快照。时钟固定：没有 TPS。
#[tokio::test(flavor = "multi_thread")]
async fn the_hit_rate_is_rebuilt_from_the_usage_rows_after_resume() {
    let model = GatedModel::scripted(vec![
        reply("one", usage(1_000, 0, 1), 1),
        reply("two", usage(200, 800, 1), 1),
    ]);
    let (release, open) = watch::channel(false);
    let setup = Setup {
        width: 120,
        session_store: Some(Arc::new(HeldList {
            inner: MemoryStore::default(),
            open,
        })),
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("first");
        ui.wait("the first reply", |s| idle(s, "cache 0%")).await;
        ui.submit("second");
        let screen = ui.wait("the second reply", |s| idle(s, "cache 40%")).await;
        assert!(screen.contains("ctx 1%"), "{screen}");
        assert!(!screen.contains("tok/s"), "{screen}");
        ui.submit("/clear");
        let screen = ui
            .wait("the cleared screen", |s| !s.contains("cache 40%"))
            .await;
        assert!(!screen.contains("ctx "), "{screen}");
        // 页面框架（标题、工具栏、`Type to search`）先于会话列表画出；列表还扣着时写 `Loading sessions…`。
        ui.submit("/resume");
        ui.wait_for("Loading sessions…").await;
        release.send_replace(true);
        // 缺陷（修复线 F31，测试的写法）：原来只等 `Type to search` 就按 enter，负载下 enter 落在列表到达之前——加载中
        // 没有选中的会话，enter 不动作（tui-manage.md §10.4），会话没有切回去，10 s 后超时。统计的查询与序号没有竞态。
        // 现在等列表的行（数据）出现再按。
        ui.wait("the session row", |s| {
            s.lines()
                .any(|line| line.starts_with("  › ") && line.ends_with("  first"))
        })
        .await;
        ui.key(KeyCode::Enter);
        let screen = ui
            .wait("the resumed session", |s| {
                !s.contains("Type to search") && s.contains("cache 40%")
            })
            .await;
        assert!(screen.contains("scripted · ctx 1% · cache 40%"), "{screen}");
    })
    .await;
}

/// 压缩结果提交后 ctx 立即按压缩后的历史更新（不等下一条回复）：150,001 / 180,000 → 83%，压缩后只剩摘要 → 0%。
#[tokio::test(flavor = "multi_thread")]
async fn the_context_share_updates_once_the_conversation_is_compacted() {
    let model = GatedModel::scripted(vec![
        reply("big reply", usage(150_000, 0, 1), 1),
        reply("<summary>short summary</summary>", usage(2_000, 0, 1), 1),
    ]);
    let setup = Setup {
        width: 120,
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("first");
        ui.wait("the big reply", |s| idle(s, "ctx 83%")).await;
        ui.submit("/compact");
        ui.wait_for("Conversation compacted").await;
        let screen = ui.wait("the new share", |s| s.contains("ctx 0%")).await;
        // 摘要请求不是主回合请求：命中率仍是第一次请求的值。
        assert!(screen.contains("ctx 0% · cache 0%"), "{screen}");
    })
    .await;
}

/// 中文界面与浅色主题：三项用中文短词，颜色是次要文字（不是 dim 的灰）。
#[tokio::test(flavor = "multi_thread")]
async fn chinese_and_light_themes_show_the_same_items() {
    let model = GatedModel::scripted(vec![reply("好的", usage(200, 800, 1), 1)]);
    let palette =
        Palette::new(ColorLevel::TrueColor, Some((255, 255, 255))).with_foreground(Some((0, 0, 0)));
    let setup = Setup {
        width: 120,
        lang: "zh_CN.UTF-8",
        palette,
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("你好");
        let screen = ui
            .wait("the reply", |s| {
                s.contains("缓存 80%") && !s.contains("esc 中断")
            })
            .await;
        assert!(
            screen.contains("scripted · 上下文 1% · 缓存 80%"),
            "{screen}"
        );
        // footer 那一行里“缓”字的格子（宽字符占两格，按格子找）。
        let buffer = ui.buffer.lock().unwrap().clone().unwrap();
        let area = buffer.area;
        let cell = (area.top()..area.bottom())
            .flat_map(|y| (area.left()..area.right()).map(move |x| (x, y)))
            .map(|at| &buffer[at])
            .find(|cell| cell.symbol() == "缓")
            .expect("the cache item");
        assert_eq!(cell.fg, palette.secondary().fg.unwrap(), "secondary text");
    })
    .await;
}
