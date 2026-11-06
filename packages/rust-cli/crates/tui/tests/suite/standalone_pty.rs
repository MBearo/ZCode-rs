#![allow(clippy::disallowed_methods, reason = "子进程按环境变量扮演哪一种情况")]
//! 启动前的两屏在真实终端里（docs/specs/rust-cli/workspace-trust.md §2.4、update.md §6，`rust_cli_tui::screens`）：子进程
//! 经伪终端进入真实终端、画出页面；页面出现之前排着的键丢掉，按键给出回答之后离开 alternate screen、回到普通模式，再把
//! 回答写到 stdout。同 `panic_terminal.rs` 的做法（只在 unix 上）。
//!
//! F138-1 的回归测试：回车紧跟在终端对探测的回答后面（同一次写入），这时页面还没有主题、还没画出来；它不算回答。旧代码上
//! 信任页把它读进来，选中焦点所在的 `No, exit`，进程以退出码 1 退出。
use std::collections::BTreeMap;
use std::io::Write;

use rust_cli_base::TaskScope;
use rust_cli_tui::screens::{self, ScreenEntry};
use rust_cli_tui::trust::TrustPrompt;
use rust_cli_tui::update::UpdateInfo;

use super::pty::{Pty, ROLE, shows};

/// 子进程的入口（[`child`]）。
const CHILD: &str = "suite::standalone_pty::child";
/// 离开 alternate screen。
const LEAVE: &str = "\x1b[?1049l";
/// ↓ 键。
const DOWN: &[u8] = b"\x1b[B";
/// 子进程写回答的前缀。
const ANSWER: &str = "ANSWER ";
/// 进入终端时的探测只有一个主要设备属性查询（kit 的 `TerminalProbe`，排在最后，它的回答结束探测）：紧跟在它的回答
/// 后面的键是在页面画出之前到的。
const AFTER_PROBE: usize = 1;

/// 回答在离开 alternate screen 之后写出，子进程正常退出，终端回到普通模式。
fn answered(pty: &mut Pty, expected: &str) {
    let (status, all) = pty.finish();
    assert!(status.success(), "{all:?}");
    let answer = all
        .find(&format!("{ANSWER}{expected}"))
        .unwrap_or_else(|| panic!("{all:?}"));
    let leave = all.rfind(LEAVE).unwrap_or_else(|| panic!("{all:?}"));
    assert!(leave < answer, "{all:?}");
    assert!(!pty.raw());
}

/// 信任页出现之前就排着的回车不算回答（F138-1）：焦点仍在 `No, exit`，之后 ↓ ⏎ 才是回答。
#[test]
fn the_trust_dialog_answers_in_a_real_terminal() {
    let mut pty = Pty::start(CHILD, "trust");
    pty.type_ahead(AFTER_PROBE, b"\r");
    pty.until("the dialog", |text| shows(text, "1  No, exit"));
    pty.send(DOWN);
    pty.send(b"\r");
    answered(&mut pty, "Trusted");
}

/// 更新选择屏出现之前就排着的回车同样丢掉（不是 Update now）。
#[test]
fn the_update_prompt_answers_in_a_real_terminal() {
    let mut pty = Pty::start(CHILD, "update");
    pty.type_ahead(AFTER_PROBE, b"\r");
    pty.until("the prompt", |text| shows(text, "1  Update now"));
    pty.send(b"3");
    answered(&mut pty, "SkipUntilNext");
}

/// 子进程的入口（只被上面的测试经伪终端启动）：英文，16 色（只有 `LANG`），没有 `theme.json`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "被伪终端的测试作为子进程启动"]
async fn child() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let scope = TaskScope::root("standalone-pty", |_| {});
    let entry = || ScreenEntry {
        vars: BTreeMap::from([("LANG".to_owned(), "en_US.UTF-8".to_owned())]),
        locale: None,
        stores: None,
        spawner: scope.spawner(),
        resize: None,
    };
    let answer = match role.as_str() {
        "trust" => {
            let prompt = TrustPrompt {
                path: "/Users/dev/work".to_owned(),
                backstop: false,
                grants: None,
            };
            let answer = screens::ask_trust(prompt, entry()).await;
            format!("{:?}", answer.unwrap())
        }
        "update" => {
            let info = UpdateInfo {
                current: "0.1.46".to_owned(),
                latest: "0.1.47".to_owned(),
                command: "npm install -g @scope/zcode".to_owned(),
                notes_url: "https://www.npmjs.com/package/@scope/zcode".to_owned(),
            };
            let choice = screens::ask_update(info, entry()).await;
            format!("{:?}", choice.unwrap())
        }
        other => panic!("unknown role {other}"),
    };
    let line = format!("\r\n{ANSWER}{answer}\r\n");
    std::io::stdout().write_all(line.as_bytes()).unwrap();
    std::io::stdout().flush().unwrap();
}
