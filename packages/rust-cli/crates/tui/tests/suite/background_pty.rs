#![allow(clippy::disallowed_methods, reason = "子进程按环境变量扮演哪一种情况")]
//! 伪终端里的在跑的命令（docs/specs/rust-cli/tui-tasks.md §2、§3.2，修复线 F117）：执行环境的替身报一条在跑的 shell 与
//! 两条在跑的 monitor（命令来源与 WebSocket 来源），子进程进入真实终端；任务行写 `1 shell, 2 monitors running`，^T 打开
//! 任务卡的 Commands，monitor 的名称是它的 description。修前（F117 之前）写 `3 shells`；2026-10-31 起没有 footer 的 pill。

use rust_cli_kernel_api::testing::text_response;

use super::ledger::Ledger;
use super::pty::{Pty, ROLE, interface_in, shows};
use super::support::GatedModel;

/// 子进程的入口（[`child`]）。
const CHILD: &str = "suite::background_pty::child";

#[test]
fn the_task_line_counts_the_monitors_in_a_real_terminal() {
    let mut pty = Pty::start(CHILD, "monitors");
    pty.until("the interface", |text| shows(text, "Ask before changes"));
    pty.send(b"go\r");
    pty.until("the reply", |text| shows(text, "watching the build"));
    // 回合结束后重取快照：一条 shell、两条 monitor（两种来源）。终端只重画变了的格子（任务行的字可能与上一帧同位置的字
    // 相同而不重写），所以用 ctrl+l 整屏重画，看重画出的任务行的整段文字；快照还没到时再重画一次。整屏重画时状态行
    // （`Ask before changes …`）在最后，读到它时上面已经画完。
    let line = "1 shell, 2 monitors running";
    let mut redrawn = String::new();
    for _ in 0..100 {
        let mark = pty.mark();
        pty.send(b"\x0c");
        redrawn = pty.until_from(mark, "a full redraw", |text| {
            shows(text, "Ask before changes")
        });
        if shows(&redrawn, line) {
            break;
        }
    }
    assert!(shows(&redrawn, line), "{redrawn:?}");
    // ^T 打开任务卡：monitor 与 shell 同在 Commands，名称是 description（不是命令原文）。终端只重写变了的格子，与上一帧
    // 同位置同字的格子被跳过（实测 `deploy feed` 写成 `dep` 加跳一格加 `oy` …），所以这里只认整段写出的 `build log`
    // 与种类一行（哪个名称被拆开随版面变）；每一行的名称在 `task_card_tests.rs` 与 `background_tests.rs` 里逐字核对。
    let mark = pty.mark();
    pty.send(b"\x14");
    let card = pty.until_from(mark, "the task card", |text| {
        shows(text, "Commands 3") && shows(text, "build log")
    });
    assert!(!shows(&card, "tail -f"), "{card:?}");
    assert!(!shows(&card, "wss://"), "{card:?}");
    // esc 关闭任务卡（等输入框回来再打字：紧跟在 esc 后面的字符会和它拼成 alt 组合键），`/exit` 退出。
    let mark = pty.mark();
    pty.send(b"\x1b");
    // 任务卡关上后输入行的槽 `? keys` 重新写出（占位的字与上一帧相同，不重写）。
    pty.until_from(mark, "the prompt", |text| shows(text, "? keys"));
    pty.send(b"/exit");
    pty.until("the command", |text| shows(text, "Quit ZCode"));
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
        Ledger(&["b1", "b2", "s3"]).registry(),
    )
    .await;
}
