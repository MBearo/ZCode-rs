#![allow(clippy::disallowed_methods, reason = "子进程按环境变量扮演哪一种情况")]
//! 伪终端里的后台任务 pill（docs/specs/rust-cli/tui-display.md §2.3、tui-panels.md §3，修复线 F117）：执行环境的替身报
//! 一条在跑的 shell 与两条在跑的 monitor（命令来源与 WebSocket 来源），子进程进入真实终端；footer 写
//! `1 shell, 2 monitors`，↓ enter 打开 Background 面板，monitor 的名称是它的 description，详情是 `Monitor details`。
//! 修前 footer 写 `3 shells`。
use std::sync::Arc;

use rust_cli_kernel_api::testing::{environments, text_response};

use super::ledger::Ledger;
use super::pty::{Pty, ROLE, interface_in, shows};
use super::support::GatedModel;

/// 子进程的入口（[`child`]）。
const CHILD: &str = "suite::background_pty::child";

#[test]
fn the_pill_counts_the_monitors_in_a_real_terminal() {
    let mut pty = Pty::start(CHILD, "monitors");
    pty.until("the interface", |text| shows(text, "Ask before changes"));
    pty.send(b"go\r");
    pty.until("the reply", |text| shows(text, "watching the build"));
    // 回合结束后重取快照：一条 shell、两条 monitor（两种来源）。终端只重画变了的格子（pill 的字可能与上一帧同位置的字
    // 相同而不重写），所以用 ctrl+l 整屏重画，看重画出的 footer 的整段文字；回合还没结束（提示还是 `esc to interrupt`）
    // 时再重画一次。整屏重画时右侧的 `ctx` 在 footer 那一行的最后，读到它时左侧已经画完。
    let footer = "· 1 shell, 2 monitors · ↓ to manage";
    let mut redrawn = String::new();
    for _ in 0..100 {
        let mark = pty.mark();
        pty.send(b"\x0c");
        redrawn = pty.until_from(mark, "a full redraw", |text| shows(text, "ctx 0.0%"));
        if shows(&redrawn, footer) {
            break;
        }
    }
    assert!(shows(&redrawn, footer), "{redrawn:?}");
    // ↓ 选中 pill，enter 打开面板：monitor 与 shell 同组，名称是 description。
    let mark = pty.mark();
    pty.send(b"\x1b[B");
    pty.until_from(mark, "the focused pill", |text| {
        shows(text, "Enter to view tasks")
    });
    let mark = pty.mark();
    pty.send(b"\r");
    let panel = pty.until_from(mark, "the panel", |text| {
        shows(text, "3 active shells") && shows(text, "deploy feed")
    });
    assert!(shows(&panel, "build log"), "{panel:?}");
    assert!(!shows(&panel, "tail -f"), "{panel:?}");
    // ↓ enter：命令来源的 monitor 的详情。
    let mark = pty.mark();
    pty.send(b"\x1b[B\r");
    let detail = pty.until_from(mark, "the monitor details", |text| {
        shows(text, "Monitor details") && shows(text, "Script: tail -f build.log")
    });
    assert!(!shows(&detail, "Shell details"), "{detail:?}");
    // esc 关闭面板（等输入框回来再打字：紧跟在 esc 后面的字符会和它拼成 alt 组合键），`/exit` 退出。
    let mark = pty.mark();
    pty.send(b"\x1b");
    pty.until_from(mark, "the prompt", |text| shows(text, "❯"));
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
    assert_eq!(role, "monitors");
    interface_in(
        GatedModel::scripted(vec![text_response(&["watching the build"])]),
        environments(Arc::new(Ledger(&["b1", "b2", "s3"]))),
    )
    .await;
}
