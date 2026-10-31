#![allow(clippy::disallowed_methods, reason = "子进程按环境变量扮演哪一种情况")]
//! 伪终端里回合结束那一行（docs/specs/rust-cli/tui-display.md §2.7，修复线 F118）：执行环境的替身报一条在跑的 monitor，
//! 子进程进入真实终端；回合结束那一行是 `✻ <动词> for 0s · done 12:00 AM · 1 monitor still running`（内核的时钟固定在
//! 0）。修前没有这一行。
use std::sync::Arc;

use rust_cli_kernel_api::testing::{environments, text_response};

use super::ledger::Ledger;
use super::pty::{Pty, ROLE, interface_in, shows};
use super::support::GatedModel;

/// 子进程的入口（[`child`]）。
const CHILD: &str = "suite::turn_duration_pty::child";

#[test]
fn the_turn_line_names_the_running_monitor_in_a_real_terminal() {
    let mut pty = Pty::start(CHILD, "monitor");
    pty.until("the interface", |text| shows(text, "Ask before changes"));
    pty.send(b"go\r");
    pty.until("the reply", |text| shows(text, "watching the build"));
    // 终端只重画变了的格子：用 ctrl+l 整屏重画，看重画出的整段文字（同 background_pty.rs）。后台的快照在回合结束后才
    // 重取，没到时再重画一次。整屏重画时右侧的 `ctx` 在 footer 那一行的最后，读到它时上面已经画完。
    let wanted = "for 0s · done 12:00 AM · 1 monitor still running";
    let mut redrawn = String::new();
    for _ in 0..100 {
        let mark = pty.mark();
        pty.send(b"\x0c");
        redrawn = pty.until_from(mark, "a full redraw", |text| shows(text, "ctx 0.0%"));
        if shows(&redrawn, wanted) {
            break;
        }
    }
    assert!(shows(&redrawn, wanted), "{redrawn:?}");
    assert!(shows(&redrawn, "· 1 monitor · ↓ to manage"), "{redrawn:?}");
    pty.send(b"/exit");
    pty.until("the command", |text| shows(text, "Exit the CLI"));
    pty.send(b"\r");
    let (status, all) = pty.finish();
    assert!(status.success(), "{all:?}");
}

/// 子进程的入口（只被上面的测试经伪终端启动）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "被伪终端的测试作为子进程启动"]
async fn child() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    assert_eq!(role, "monitor");
    interface_in(
        GatedModel::scripted(vec![text_response(&["watching the build"])]),
        environments(Arc::new(Ledger(&["b2"]))),
    )
    .await;
}
