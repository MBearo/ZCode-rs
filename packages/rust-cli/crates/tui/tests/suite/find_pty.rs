#![allow(clippy::disallowed_methods, reason = "子进程按环境变量扮演哪一种情况")]
//! 伪终端里的搜索模式（docs/specs/rust-cli/tui-input.md §2.3，修复线 F114）：子进程进入真实终端，手打 `//` 进入搜索
//! 模式，查询、计数与当前匹配的高亮写到终端，esc 退出；括号粘贴进来的 `//` 不切模式。
//!
//! 终端只重画变了的格子：断言只看某个时刻之后的输出（`Pty::mark`），等的是这一步新画出来的字。
use rust_cli_kernel_api::testing::text_response;

use super::pty::{Pty, ROLE, interface, shows};
use super::support::GatedModel;

/// 子进程的入口（[`child`]）。
const CHILD: &str = "suite::find_pty::child";
/// 16 色终端里当前匹配的黄底：plain 主题的 `find_current` 是 `code`（ANSI 黄，SGR 33）的前景加反显、粗体加下划线，显示为
/// 黄底（tui-kit.md §3.4、§4.3）。kit 的渲染器每个样式写自含的 SGR（先 `0` 重置）。
const CURRENT: &str = "\x1b[0;1;4;7;33m";
/// 其余的匹配：反显。
const REVERSED: &str = "\x1b[0;7m";

#[test]
fn typed_slashes_search_in_a_real_terminal() {
    let mut pty = Pty::start(CHILD, "find");
    pty.until("the interface", |text| shows(text, "Ask before changes"));
    pty.send(b"go\r");
    pty.until("the reply", |text| shows(text, "alpha beta alpha"));
    // 两个 `/` 一次写进去：终端照样一个键一个键地送来，是手打的。
    let mark = pty.mark();
    pty.send(b"//");
    pty.until_from(mark, "the search box", |text| shows(text, "⌕"));
    // 查询一次粘贴进来（逐个字符打时计数一个字一个字地变，终端上只重画变了的数字）：计数与当前匹配的黄底一起画出来。
    let mark = pty.mark();
    pty.send(b"\x1b[200~alpha\x1b[201~");
    let shown = pty.until_from(mark, "the count", |text| shows(text, "2/2"));
    assert!(shows(&shown, "alpha"), "{shown:?}");
    assert!(shown.contains(CURRENT), "{shown:?}");
    assert!(shown.contains(REVERSED), "{shown:?}");
    // esc：退出搜索，输入行的记号回到 `›`（Puffin 的输入框，tui-kit.md §4.2）。
    let mark = pty.mark();
    pty.send(b"\x1b");
    let shown = pty.until_from(mark, "the prompt", |text| shows(text, "›"));
    assert!(!shows(&shown, "⌕"), "{shown:?}");
    // 括号粘贴的 `//` 是输入框里的文字。
    let mark = pty.mark();
    pty.send(b"\x1b[200~//\x1b[201~");
    let shown = pty.until_from(mark, "the pasted text", |text| shows(text, "//"));
    assert!(!shows(&shown, "⌕"), "{shown:?}");
    // ctrl+c 清掉输入，`/exit` 退出。
    pty.send(b"\x03");
    pty.send(b"/exit");
    // 命令架上 `/exit` 的说明照 Puffin（`Quit ZCode.`；以前是 `Exit the CLI`，等不到它时测试在按 ⏎ 之前就超时了）。
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
    assert_eq!(role, "find");
    interface(GatedModel::scripted(vec![text_response(&[
        "alpha beta alpha",
    ])]))
    .await;
}
