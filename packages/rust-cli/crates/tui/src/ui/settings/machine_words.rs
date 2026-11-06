//! 远端机器的英文（设置的 Machines 段；照抄 Puffin `shell/kept/machine_words.rs`，两套界面不能互相依赖）。设置页同 Puffin
//! 只有英文（tui-kit.md §4.3）。
use rust_cli_base::names::{DOT_DIR, EXECUTOR_HOME, UNIX_EXECUTABLE};
use rust_cli_kernel_api::metering::format_duration;

use crate::machines::{ConnectStep, MachineFailure, MachineStatus};

/// 列表的字在 kit 的 `machine_rows` 里（docs/specs/rust-cli/tui-kit.md §4.4 K4），这里原路径转出。
pub use rust_cli_tui_kit::app::widgets::machine_rows::{
    ADD, COULD_NOT_CONNECT, ENV_NOT_REFRESHED, NOT_CONNECTED, REMOTE_HOME,
};
pub const UNAVAILABLE: &str = "Remote machines aren't available here.";

pub fn executor(version: &str) -> String {
    format!("executor {version}")
}

/// 执行器的系统名（`linux` → `Linux`）。
pub fn os_name(os: &str) -> String {
    match os {
        "linux" => "Linux".to_owned(),
        "macos" => "macOS".to_owned(),
        "windows" => "Windows".to_owned(),
        other => other.to_owned(),
    }
}

/// 列表与设置的段里行尾的状态词（就绪写延迟，另算）。
pub fn state_word(status: &MachineStatus) -> &'static str {
    match status {
        MachineStatus::Ready => "connected",
        MachineStatus::Connecting(step) => step_word(*step),
        MachineStatus::Reconnecting { .. } => "reconnecting",
        MachineStatus::Failed(_) => COULD_NOT_CONNECT,
        MachineStatus::Disconnected => NOT_CONNECTED,
    }
}

/// 第一次连接的这一步（F98、F100，remote-exec.md §21.3）。
pub fn step_word(step: ConnectStep) -> &'static str {
    match step {
        ConnectStep::Checking => "checking",
        ConnectStep::Downloading => "downloading the executor",
        ConnectStep::Uploading { .. } => "uploading the executor",
        ConnectStep::Handshaking => "handshaking",
    }
}

/// 分步里走过的一步与这一步的写法（`✓ checked`、`○ uploading 89 MB`）：`done` 是走过了。
pub fn step_done(step: ConnectStep, done: bool) -> String {
    match (step, done) {
        (ConnectStep::Checking, true) => "checked".to_owned(),
        (ConnectStep::Checking, false) => "checking".to_owned(),
        (ConnectStep::Downloading, true) => "downloaded the executor".to_owned(),
        (ConnectStep::Downloading, false) => "downloading the executor".to_owned(),
        (ConnectStep::Uploading { bytes }, true) => format!("uploaded {} MB", megabytes(bytes)),
        (ConnectStep::Uploading { bytes }, false) => format!("uploading {} MB", megabytes(bytes)),
        (ConnectStep::Handshaking, _) => "handshake".to_owned(),
    }
}

/// 从开始连到现在：向下取整到秒（`14s`）。
pub fn elapsed(ms: u64) -> String {
    format_duration(ms / 1000 * 1000)
}

/// ⏎（列表）与动作（设置的机器页）随状态做的事（remote-exec.md §21.3）。
pub fn action_word(status: &MachineStatus) -> &'static str {
    match status {
        MachineStatus::Ready => "disconnect",
        MachineStatus::Reconnecting { .. } => "retry now",
        MachineStatus::Connecting(_) => "stop",
        MachineStatus::Failed(_) => "try again",
        MachineStatus::Disconnected => "connect",
    }
}

/// 字节数按 MiB 四舍五入、至少 1。
fn megabytes(bytes: u64) -> u64 {
    ((bytes + (1 << 19)) >> 20).max(1)
}

/// 重连中的倒计时（F96）：`next try in 12s`；正在连时没有。
pub fn next_try(left_ms: Option<u64>) -> Option<String> {
    left_ms.map(|ms| format!("next try in {}", crate::ui::retry_status::countdown(ms)))
}

/// 列表里重连中的机器的第三行：最近一次的原因（F96）。
pub fn last_error(reason: &str) -> String {
    format!("Last error: {}", explain(reason))
}

/// 设置页添加之后输入框上方的一句（remote-exec.md §21.3）。
pub fn added_short(name: &str) -> String {
    format!("Added {name}. Connecting…")
}

/// 失败的原因（列表与设置页的第一句）：ssh 一类的写成人话加句号，其余照 §21.4 的整句。
pub fn cause(name: &str, failure: &MachineFailure) -> String {
    match failure {
        MachineFailure::Unreachable(m) | MachineFailure::Lost(m) => sentence(&explain(m)),
        other => failure_text(name, other),
    }
}

/// 列表里原因下面的建议（fg3 与 fg2 的几段，fg2 是要跑的命令）；没有时空。
pub fn advice(failure: &MachineFailure, ssh: &str) -> Vec<(String, bool)> {
    match failure {
        MachineFailure::Unreachable(_) | MachineFailure::Lost(_) => vec![
            ("Is the machine on? ".to_owned(), false),
            (format!("ssh {ssh}"), true),
            (
                " in another terminal shows what ssh sees.".to_owned(),
                false,
            ),
        ],
        MachineFailure::Incompatible { busy: true, .. } => {
            vec![("Connect again once its work ends.".to_owned(), false)]
        }
        _ => Vec::new(),
    }
}

/// 设置的机器页的说明：这台的处境（remote-exec.md §21.3）；没事时没有。
pub fn situation(name: &str, ssh: &str, status: &MachineStatus) -> String {
    match status {
        MachineStatus::Failed(MachineFailure::Unreachable(m) | MachineFailure::Lost(m)) => {
            format!(
                "{name} couldn't connect: {} Check that it is on and that ssh {ssh} works in a terminal.",
                sentence(&explain(m))
            )
        }
        MachineStatus::Failed(other) => failure_text(name, other),
        MachineStatus::Reconnecting { reason, .. } if !reason.is_empty() => format!(
            "{name} lost its connection and is reconnecting: {}",
            sentence(&explain(reason))
        ),
        _ => String::new(),
    }
}

/// 句子以句号结尾。
fn sentence(text: &str) -> String {
    if text.ends_with(['.', '!', '?']) {
        text.to_owned()
    } else {
        format!("{text}.")
    }
}

/// 从 ssh 目标取的名字（remote-exec.md §21.3）：去掉 `user@` 与 `:端口`，取主机名的第一段；不合名字规则的字换成 `-`，
/// 截到 32 个字符。`ubuntu@gpu-2.example.com` → `gpu-2`。
pub fn name_from(target: &str) -> String {
    let host = target.trim().rsplit('@').next().unwrap_or_default();
    let host = host.split(':').next().unwrap_or_default();
    // IP 地址没有能当名字的第一段：整个留下。
    let first = if host.chars().all(|c| c.is_ascii_digit() || c == '.') {
        host
    } else {
        host.split('.').next().unwrap_or_default()
    };
    first
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .take(32)
        .collect()
}

/// 错误链里内部的前缀（可以叠着出现）：去掉它们，留下最里面的原因（F98，remote-exec.md §25.4.4）。
const INTERNAL: [&str; 5] = [
    "cannot reach the executor: ",
    "the executor connection was lost: ",
    "link io error: ",
    "cannot connect: ",
    "executor exited (",
];

/// 去掉内部的前缀，取最后一行非空的。
fn innermost(reason: &str) -> &str {
    let mut rest = reason.trim();
    while let Some(prefix) = INTERNAL.iter().find(|prefix| rest.starts_with(**prefix)) {
        let after = &rest[prefix.len()..];
        // `executor exited (exit status: 255): …`：去到右括号与冒号之后。
        rest = if *prefix == "executor exited (" {
            match after.split_once("): ") {
                Some((_, tail)) => tail,
                None => break,
            }
        } else {
            after
        }
        .trim_start();
    }
    rest.lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(rest)
}

/// 连接错误的原因写成人话（F98，remote-exec.md §25.4.4）：去掉内部的链，认得的几种写成人话，其余留下最里面的原文。
pub fn explain(reason: &str) -> String {
    let inner = innermost(reason);
    let reach = |rest: &str| -> Option<String> {
        let rest = rest.strip_prefix("ssh: connect to host ")?;
        let (host, rest) = rest.split_once(" port ")?;
        let (port, why) = rest.split_once(": ")?;
        Some(format!("ssh can't reach {host}:{port} ({why})"))
    };
    let resolve = |rest: &str| -> Option<String> {
        let rest = rest.strip_prefix("ssh: Could not resolve hostname ")?;
        let (host, why) = rest.split_once(": ")?;
        Some(format!("ssh can't find the host {host} ({why})"))
    };
    let closed = |rest: &str| -> Option<String> {
        let rest = rest.strip_prefix("Connection closed by ")?;
        let (host, port) = rest.split_once(" port ")?;
        Some(format!("{host}:{} closed the ssh connection", port.trim()))
    };
    let silent = |rest: &str| -> Option<String> {
        let ms = rest
            .strip_prefix("no frame from the executor for ")?
            .strip_suffix(" ms")?;
        Some(format!("no reply from the executor for {ms} ms"))
    };
    [reach, resolve, closed, silent]
        .iter()
        .find_map(|known| known(inner))
        .unwrap_or_else(|| inner.to_owned())
}

/// 连接错误的一句话（§21.4）：连接命令的回执与列表里失败的机器共用。
pub fn failure_text(name: &str, failure: &MachineFailure) -> String {
    match failure {
        MachineFailure::Unreachable(m) => {
            format!("Couldn't reach {name} over SSH: {}", explain(m))
        }
        MachineFailure::Stopped => format!("Stopped connecting {name}."),
        MachineFailure::Unsupported { os, arch } => {
            format!(
                "{name} runs {os} {arch}; remote machines must be Linux x86_64 / aarch64 or macOS arm64."
            )
        }
        MachineFailure::NoArtifact { version, target } => format!(
            "There's no zcode {version} build for {target} to upload to {name}, and no compatible executor is installed there."
        ),
        MachineFailure::Incompatible {
            version,
            protocol,
            busy: true,
        } => format!(
            "The executor on {name} is zcode {version} (protocol {protocol}) and is busy, so it can't be replaced now. Connect again once its work ends, or stop it on {name} with `zcode-rs executor stop` (its background tasks end)."
        ),
        MachineFailure::Incompatible {
            version, protocol, ..
        } => format!(
            "The executor on {name} is zcode {version} (protocol {protocol}), which this zcode can't use."
        ),
        MachineFailure::Upload(m) => format!("Uploading the executor to {name} failed: {m}"),
        MachineFailure::Probe(m) => {
            format!("Couldn't understand {name}'s reply while checking it: {m}")
        }
        MachineFailure::DownloadFailed {
            version,
            target,
            reason,
        } => format!(
            "Couldn't download the zcode {version} executor ({target}) to upload to {name}: {reason}. No compatible executor is installed there either."
        ),
        MachineFailure::Musl => format!(
            "{name} uses musl libc (for example Alpine), and there's no zcode executor package for musl. Use a glibc-based Linux such as Debian or Ubuntu."
        ),
        MachineFailure::OldGlibc { glibc, needs } => {
            let has = glibc.as_ref().map_or_else(
                || format!("{name}'s glibc is older"),
                |version| format!("{name} has glibc {version}"),
            );
            format!(
                "The zcode executor can't run on {name}: it needs {needs}, but {has}. Upgrade that machine's system or use a newer Linux."
            )
        }
        MachineFailure::LatestIncompatible {
            version,
            protocol,
            latest,
            latest_protocol,
            target,
        } => format!(
            "This zcode ({version}, protocol {protocol}) isn't on npm, and the latest executor on npm, zcode {latest}, speaks protocol {latest_protocol}, so there's no executor for {name}. Connect again after a new version is published, or put a zcode {version} executor at ~/{DOT_DIR}/{EXECUTOR_HOME}/dist/{version}/{target}/{UNIX_EXECUTABLE} on this computer."
        ),
        // F107（remote-exec.md §28.6）：这个平台没有包（Intel Mac）、上传的执行器被系统杀掉（签名无效）。文字同旧 tui。
        MachineFailure::NoPackage { package } => format!(
            "There's no zcode executor package for {name} (it would be {package}), and no compatible executor is installed there. Install zcode on that machine with `npm i -g @mbearo/zcode`, then connect again."
        ),
        MachineFailure::Unsigned => format!(
            "The uploaded zcode executor was killed on {name}: macOS refuses to run it because its signature is not valid. Install zcode on that machine with `npm i -g @mbearo/zcode`, or sign the file there with `codesign -s -`."
        ),
        MachineFailure::Refused(m) => format!("The executor on {name} refused the connection: {m}"),
        MachineFailure::Lost(m) => format!("Lost the connection to {name}: {}", explain(m)),
    }
}
