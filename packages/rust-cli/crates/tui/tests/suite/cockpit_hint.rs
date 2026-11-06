//! 槽里驾驶舱的提示（tui-cockpit.md §3.4）：监听一直开着，页关着时每一项重读槽的数；页关上时也读一次。笔记与变化来自
//! `CockpitPort` 的替身。
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crossterm::event::KeyCode;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use futures_util::stream::{BoxStream, StreamExt};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::peers::TmuxPlace;
use rust_cli_kernel_api::testing::MemoryStore;
use rust_cli_tui::services::{CockpitNote, CockpitPort};
use tokio::sync::mpsc;

use super::sessions_seed::{Seed, seed};
use super::support::{GatedModel, Setup, with_ui};

const SEEDS: [Seed; 2] = [
    Seed {
        id: "sess_alpha",
        root: "/Users/dev/project",
        title: "Alpha task",
        prompt: "alpha prompt",
        reply: "alpha reply",
        created: 1_000_000,
        updated: 2_000_000,
    },
    Seed {
        id: "sess_other",
        root: "/Users/dev/other",
        title: "Other place",
        prompt: "other prompt",
        reply: "other reply",
        created: 1_000_000,
        updated: 1_000_000,
    },
];

/// 笔记在 `notes` 里；`changes` 的流是测试手里的发送端（建好时先出一项，同入口的监听）。
struct Port {
    notes: Mutex<HashMap<String, CockpitNote>>,
    changes: Mutex<Option<mpsc::UnboundedReceiver<()>>>,
}

impl Port {
    fn note(&self, id: &str, note: CockpitNote) {
        self.notes.lock().unwrap().insert(id.to_owned(), note);
    }
}

impl CockpitPort for Port {
    fn changes(&self) -> BoxStream<'static, ()> {
        // runner 只取一次；再取时没有项。
        let Some(mut receiver) = self.changes.lock().unwrap().take() else {
            return futures_util::stream::pending().boxed();
        };
        futures_util::stream::once(async {})
            .chain(futures_util::stream::poll_fn(move |cx| {
                receiver.poll_recv(cx)
            }))
            .boxed()
    }

    fn notes(&self, ids: Vec<SessionId>) -> BoxFuture<'static, HashMap<SessionId, CockpitNote>> {
        let notes = self.notes.lock().unwrap();
        let found = ids
            .into_iter()
            .filter_map(|id| notes.get(id.as_str()).cloned().map(|note| (id, note)))
            .collect();
        async move { found }.boxed()
    }

    fn mark_seen(&self, _id: SessionId, _at_ms: u64) -> BoxFuture<'static, ()> {
        async {}.boxed()
    }

    fn write_turn(&self, _: SessionId, _: u64, _: Option<String>) -> BoxFuture<'static, ()> {
        async {}.boxed()
    }

    fn forget(&self, _id: SessionId) -> BoxFuture<'static, ()> {
        async {}.boxed()
    }

    fn go_to(&self, _place: TmuxPlace) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn tmux_socket(&self) -> Option<String> {
        None
    }
}

fn ended(at: u64, failed: bool) -> CockpitNote {
    CockpitNote {
        ended_at_ms: Some(at),
        failed: failed.then(|| "boom".to_owned()),
        seen_at_ms: None,
    }
}

/// 别的会话结束了没看：槽是 `← ○ 1 new  ? keys`；失败了的比它急；页开着时的变化给页，关上时再读一次；看过了说明回到
/// `agent cockpit`；有草稿时没有提示。
#[tokio::test(flavor = "multi_thread")]
async fn the_slot_counts_news_from_the_watch_while_the_cockpit_is_closed() {
    let store = Arc::new(MemoryStore::default());
    seed(&store, &SEEDS).await;
    let (send, receive) = mpsc::unbounded_channel();
    let port = Arc::new(Port {
        notes: Mutex::new(HashMap::new()),
        changes: Mutex::new(Some(receive)),
    });
    port.note("sess_other", ended(5, false));
    let setup = Setup {
        width: 120,
        height: 30,
        store,
        services: rust_cli_tui::Services {
            cockpit: Some(port.clone()),
            ..super::stub_services::services()
        },
        ..Setup::default()
    };
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            // 建好监听时的第一项。
            ui.wait_for("← ○ 1 new  ? keys").await;
            // 页关着：每一项重读。
            port.note("sess_alpha", ended(6, true));
            send.send(()).unwrap();
            ui.wait_for("← ✗ 1 failed  ? keys").await;
            // 有草稿时 ← 移动光标，没有提示。
            ui.type_text("x");
            ui.wait("the hint to leave", |s| {
                !s.contains("1 failed") && !s.contains("agent cockpit")
            })
            .await;
            ui.key(KeyCode::Backspace);
            ui.wait_for("← ✗ 1 failed  ? keys").await;
            // 页开着：变化给页；关上时再读一次。
            ui.key(KeyCode::Left);
            ui.wait_for("Agent cockpit").await;
            let seen = CockpitNote {
                seen_at_ms: Some(9),
                ..ended(6, true)
            };
            port.note("sess_alpha", seen);
            send.send(()).unwrap();
            ui.wait("the page to reread", |s| !s.contains("1 failed"))
                .await;
            ui.key(KeyCode::Esc);
            let screen = ui
                .wait("back with the new count", |s| {
                    !s.contains("Agent cockpit") && s.contains("← ○ 1 new  ? keys")
                })
                .await;
            assert!(!screen.contains("failed"), "{screen}");
            // 都看过了：槽照旧。两项连着来，最后的也读到。
            port.note(
                "sess_other",
                CockpitNote {
                    seen_at_ms: Some(9),
                    ..ended(5, false)
                },
            );
            send.send(()).unwrap();
            send.send(()).unwrap();
            ui.wait_for("← agent cockpit  ? keys").await;
        },
    )
    .await;
}
