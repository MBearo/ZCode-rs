//! Windows 的真实进程（工作线 F66）：Bash 工具经 Git Bash（docs/specs/rust-cli/shell-permissions.md §2.1）、command
//! hook（hooks.md §5.1，Windows 上也经 Git Bash）、`-p` 下用户中断的退出码（session-lifecycle.md §3.5）、更新的交接
//! （update.md §8、§8.1）。只在 Windows runner 上运行，需要 Git for Windows（Git Bash）；开发机上只做了交叉编译检查
//! （`node scripts/rust-cli/clippy.mjs --target windows`，performance.md §8）。
//!
//! 超时只用于测试判定，不参与被测的同步。
#![cfg(windows)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、读回写出的文件、取测试进程的系统变量"
)]

mod sse;
mod system_env;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use rust_cli_base::models::GLM_5_3;
use serde_json::{Value, json};
use system_env::KeepSystem as _;

const DEADLINE: Duration = Duration::from_secs(60);
/// 找 Git Bash 要的变量（`PATH` 里的 git，或显式的 `ZCODE_GIT_BASH_PATH`），从测试进程带过去。
const GIT_BASH_VARS: &[&str] = &["PATH", "ZCODE_GIT_BASH_PATH"];

/// 临时的 HOME 与工作区（工作区有 `.git`，是项目根）。
struct Sandbox {
    _dir: tempfile::TempDir,
    base: PathBuf,
    home: PathBuf,
    work: PathBuf,
}

fn sandbox() -> Sandbox {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().to_path_buf();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(home.join(".zcode").join("cli")).unwrap();
    Sandbox {
        _dir: dir,
        base,
        home,
        work,
    }
}

impl Sandbox {
    /// 用户级配置（Node 配置文件的形状，config.md §2）。
    fn config(&self, value: &Value) {
        std::fs::write(
            self.home.join(".zcode").join("cli").join("config.json"),
            value.to_string(),
        )
        .unwrap();
    }

    /// 起 `zcode-rs` 的命令：只有系统变量、找 Git Bash 的变量与本测试设的；`model` 是本地模型夹具的地址。
    fn zcode(&self, args: &[&str], model: Option<&str>) -> Command {
        let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
        command
            .args(args)
            .current_dir(&self.work)
            .env_clear()
            .keep_system()
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("ZCODE_DATA_BASE_DIR", &self.home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in GIT_BASH_VARS {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        if let Some(url) = model {
            command
                .env("ZCODE_MODEL_BASE_URL", url)
                .env("ZCODE_API_KEY", "test-key")
                .env("ZCODE_MODEL", GLM_5_3);
        }
        command
    }
}

/// Git Bash 里的写法：`C:\x\y` → `C:/x/y`（Git Bash 认，单引号里不转义）。
fn bash_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// 请求里这个工具调用的 tool_result 的文字。
fn tool_result(request: &Value, id: &str) -> String {
    let mut out = String::new();
    for message in request["messages"].as_array().unwrap() {
        for block in message["content"].as_array().into_iter().flatten() {
            if block["type"] != "tool_result" || block["tool_use_id"] != id {
                continue;
            }
            match &block["content"] {
                Value::String(text) => out.push_str(text),
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .for_each(|part| out.push_str(part)),
                _ => {}
            }
        }
    }
    out
}

fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let started = Instant::now();
    while !check() {
        assert!(started.elapsed() < DEADLINE, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Bash 工具经 Git Bash 执行（Windows 上只用 Git Bash，shell-permissions.md §2.5）：命令的输出回到第二个请求的 tool_result，
/// `uname` 是 Git Bash 的（MINGW / MSYS）。
#[test]
fn the_bash_tool_runs_through_git_bash() {
    let sandbox = sandbox();
    let bash =
        json!({"command": "echo windows-bash-ok; uname -s", "description": "probe"}).to_string();
    let fixture = sse::serve(vec![
        sse::tools_reply(&[sse::tool_block(0, "toolu_probe", "Bash", &bash)]),
        sse::text_reply("done"),
    ]);
    let output = sandbox
        .zcode(
            &["-p", "go", "--permission-mode", "bypassPermissions"],
            Some(&fixture.url),
        )
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let requests = fixture.main_requests();
    assert_eq!(requests.len(), 2, "{output:?}");
    let result = tool_result(&requests[1], "toolu_probe");
    assert!(result.contains("windows-bash-ok"), "{result}");
    assert!(
        result.contains("MINGW") || result.contains("MSYS"),
        "not Git Bash: {result}"
    );
}

/// command hook 经 Git Bash 执行：UserPromptSubmit 的 hook 收到 stdin 的 JSON，纯文本 stdout 作为
/// `UserPromptSubmit hook success` 进第一个请求（hooks.md §4.5）。
#[test]
fn a_command_hook_runs_through_git_bash() {
    let sandbox = sandbox();
    let input = sandbox.base.join("hook-input.json");
    let hook = format!("cat > '{}'; echo hook-context-marker", bash_path(&input));
    sandbox.config(
        &json!({"hooks": {"enabled": true, "events": {"UserPromptSubmit": [{"hooks": [
            {"type": "command", "command": hook}
        ]}]}}}),
    );
    let fixture = sse::serve(vec![sse::text_reply("done")]);
    let output = sandbox
        .zcode(
            &["-p", "hello hook", "--permission-mode", "bypassPermissions"],
            Some(&fixture.url),
        )
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let received: Value = serde_json::from_str(&std::fs::read_to_string(&input).unwrap()).unwrap();
    assert_eq!(received["hook_event_name"], "UserPromptSubmit");
    assert_eq!(received["prompt"], "hello hook");
    let requests = fixture.main_requests();
    assert!(
        requests[0].to_string().contains("hook-context-marker"),
        "{}",
        requests[0]
    );
}

/// 让测试进程有控制台（Windows runner 的服务进程可能没有），之后起的子进程继承它，才能收到控制台事件。已经有控制台时
/// AllocConsole 失败，不影响。
fn ensure_console() {
    // SAFETY: AllocConsole 没有参数，只改本进程的控制台归属；失败时（已经有控制台）返回 0，忽略。
    #[expect(unsafe_code, reason = "测试：给本进程分配控制台（Win32 API）")]
    unsafe {
        windows_sys::Win32::System::Console::AllocConsole();
    }
}

/// 给子进程所在的进程组发 Ctrl+Break。Ctrl+C 不能只发给一个进程组（会发给同一控制台上的所有进程，包括测试进程），
/// zcode 把 Ctrl+Break 与 Ctrl+C 一样当作用户中断（`signals.rs`，session-lifecycle.md §3.5 D1）。
fn ctrl_break(group: u32) -> bool {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
    // SAFETY: 只传值参数；group 是用 CREATE_NEW_PROCESS_GROUP 起的子进程的 pid（即它的进程组 ID）。
    #[expect(unsafe_code, reason = "测试：发控制台事件（Win32 API）")]
    let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, group) };
    sent != 0
}

/// `-p` 的 Bash 在 `sleep` 时收到用户中断：中止回合，以 0 退出（同 Unix 的 SIGINT，session-lifecycle.md §3.5），日志里有
/// `process.signal` 与 `process.exit`。以前经 npm 启动器时启动器把 Ctrl+C 转成 TerminateProcess，退出码是 1（F66
/// 改了启动器，见 session-lifecycle.md §3.5.1；这里直接起 zcode-rs）。
#[test]
fn a_user_interrupt_during_print_mode_exits_with_zero() {
    use std::os::windows::process::CommandExt as _;
    /// `CREATE_NEW_PROCESS_GROUP`：子进程自成一个进程组，Ctrl+Break 只发给它。
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    ensure_console();
    let sandbox = sandbox();
    let started = sandbox.base.join("SLEEP_STARTED");
    let bash = json!({
        "command": format!("echo started > '{}'; sleep 30", bash_path(&started)),
        "description": "wait",
    })
    .to_string();
    let fixture = sse::serve(vec![
        sse::tools_reply(&[sse::tool_block(0, "toolu_sleep", "Bash", &bash)]),
        sse::text_reply("done"),
    ]);
    let mut child = sandbox
        .zcode(
            &["-p", "go", "--permission-mode", "bypassPermissions"],
            Some(&fixture.url),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .unwrap();
    wait_for("the Bash tool to start sleep", || started.exists());
    assert!(ctrl_break(child.id()), "GenerateConsoleCtrlEvent failed");
    let mut status = None;
    wait_for("zcode to exit", || {
        status = child.try_wait().unwrap();
        status.is_some()
    });
    assert_eq!(status.unwrap().code(), Some(0));
    let log = sandbox.home.join(".zcode").join("rust-cli").join("log");
    let lines: String = std::fs::read_dir(&log)
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect();
    assert!(
        lines.contains("CTRL_BREAK") && lines.contains("\"process.exit\""),
        "{lines}"
    );
}

/// 更新（update.md §8、§8.1）：有标记、没有交接变量（不是经启动器启动）时不执行，报出要用户自己执行的命令、退出码 1；
/// 有交接变量时只写交接文件、以 0 退出。两种都不起包管理器（PATH 里假的 `npm.cmd` 被调用时会写日志）。
#[test]
fn the_update_is_handed_to_the_launcher() {
    let sandbox = sandbox();
    let bin = sandbox.base.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let calls = sandbox.base.join("calls.log");
    std::fs::write(
        bin.join("npm.cmd"),
        format!("@echo npm %* >> \"{}\"\r\n", calls.display()),
    )
    .unwrap();
    let markers = [
        ("ZCODE_MANAGED_BY_NPM", "1"),
        ("ZCODE_MANAGED_PACKAGE", "@scope/zcode"),
        ("ZCODE_MANAGED_PLATFORM_PACKAGE", "@scope/zcode-win32-x64"),
    ];
    let run = |extra: &[(&str, &str)]| -> Output {
        let mut command = sandbox.zcode(&["update"], None);
        command
            .env("PATH", &bin)
            .envs(markers)
            .envs(extra.iter().copied());
        command.output().unwrap()
    };
    let output = run(&[]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        text(&output.stderr),
        "Error: Cannot update ZCode while it is running on Windows. Please run `npm install -g @scope/zcode` after it exits.\n"
    );
    assert!(output.stdout.is_empty(), "{output:?}");
    let handoff = sandbox.base.join("zcode-update-handoff.json");
    let handoff_text = handoff.display().to_string();
    let output = run(&[("ZCODE_MANAGED_UPDATE_HANDOFF", &handoff_text)]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        output.stdout.is_empty() && output.stderr.is_empty(),
        "{output:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&handoff).unwrap(),
        "{\"version\":\"latest\"}\n"
    );
    assert!(!calls.exists(), "the package manager ran");
}
