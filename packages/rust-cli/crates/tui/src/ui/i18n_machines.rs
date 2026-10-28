//! 远端机器的文字（docs/specs/rust-cli/remote-exec.md §21.3、§21.4、§21.5）：`/machines` 的列表与回执、连接错误的说明、
//! 状态行的状态词。中英文同一张表（照 `i18n_*` 的写法）。
use super::i18n::Lang;
use crate::machines::{ConnectStep, MachineFailure, MachineProblem, MachineStatus};

fn pick(lang: Lang, en: &'static str, zh: &'static str) -> &'static str {
    match lang {
        Lang::En => en,
        Lang::Zh => zh,
    }
}

pub fn title(lang: Lang) -> &'static str {
    pick(lang, "Machines", "远端机器")
}

/// 列表的副标题：`2 machines · 1 connected`；没有机器时 `No machines yet`。
pub fn summary(lang: Lang, total: usize, connected: usize) -> String {
    match (lang, total) {
        (Lang::En, 0) => "No machines yet".to_owned(),
        (Lang::Zh, 0) => "还没有机器".to_owned(),
        (Lang::En, 1) => format!("1 machine · {connected} connected"),
        (Lang::En, n) => format!("{n} machines · {connected} connected"),
        (Lang::Zh, n) => format!("{n} 台机器 · {connected} 台已连接"),
    }
}

pub fn empty_hint(lang: Lang) -> &'static str {
    pick(
        lang,
        "Add one with /machines add <name> <ssh target> [working directory]",
        "用 /machines add <名字> <ssh 目标> [工作目录] 添加",
    )
}

/// 没写工作目录（远端的 home）。
pub fn remote_home(lang: Lang) -> &'static str {
    pick(lang, "remote home", "远端 home")
}

pub fn executor(lang: Lang, version: &str) -> String {
    match lang {
        Lang::En => format!("executor {version}"),
        Lang::Zh => format!("执行器 {version}"),
    }
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

/// 列表里的状态词（就绪写 `connected`）。
pub fn status_word(lang: Lang, status: &MachineStatus) -> &'static str {
    match status {
        MachineStatus::Ready => pick(lang, "connected", "已连接"),
        other => short_word(lang, other).unwrap_or_default(),
    }
}

/// “重连中”：状态行的词；Background 面板里重连中的机器上一份的条目也用它（F96，remote-exec.md §23.3）。
pub fn reconnecting(lang: Lang) -> &'static str {
    pick(lang, "reconnecting", "重连中")
}

/// 状态行里名字后面的词；就绪时没有（写延迟）。
pub fn short_word(lang: Lang, status: &MachineStatus) -> Option<&'static str> {
    Some(match status {
        MachineStatus::Ready => return None,
        MachineStatus::Connecting(_) => pick(lang, "connecting", "连接中"),
        MachineStatus::Reconnecting { .. } => reconnecting(lang),
        MachineStatus::Failed(_) => pick(lang, "failed", "连接失败"),
        MachineStatus::Disconnected => pick(lang, "disconnected", "未连接"),
    })
}

/// 第一次连接的哪一步（F98，remote-exec.md §25.4.2；F100 加下载，§26.6）：下载、上传与握手接在“连接中”后面；探测时没有。
pub fn connect_step(lang: Lang, step: ConnectStep) -> Option<String> {
    match (lang, step) {
        (_, ConnectStep::Checking) => None,
        (Lang::En, ConnectStep::Downloading) => Some(" · downloading the executor".to_owned()),
        (Lang::Zh, ConnectStep::Downloading) => Some(" · 下载执行器".to_owned()),
        (Lang::En, ConnectStep::Uploading { bytes }) => Some(format!(
            " · uploading the executor ({} MB)",
            megabytes(bytes)
        )),
        (Lang::Zh, ConnectStep::Uploading { bytes }) => {
            Some(format!(" · 上传执行器（{} MB）", megabytes(bytes)))
        }
        (Lang::En, ConnectStep::Handshaking) => Some(" · handshaking".to_owned()),
        (Lang::Zh, ConnectStep::Handshaking) => Some(" · 握手".to_owned()),
    }
}

/// 字节数按 MiB 四舍五入、至少 1。
fn megabytes(bytes: u64) -> u64 {
    ((bytes + (1 << 19)) >> 20).max(1)
}

/// 工具行：调用在重连中的机器上等（F98，remote-exec.md §25.4.1）。
pub fn waiting_reconnect(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("Waiting for {name} to reconnect…"),
        Lang::Zh => format!("等待 {name} 重连…"),
    }
}

/// “立即重试”的回执（F98）。
pub fn retrying(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("Retrying {name} now."),
        Lang::Zh => format!("马上重试 {name}。"),
    }
}

/// 没在等重连（正在连、没断开），立即重试不做事（F98）。
pub fn not_waiting(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("{name} isn't waiting to reconnect."),
        Lang::Zh => format!("{name} 没有在等重连。"),
    }
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

/// 连接错误的原因写成人话（F98，remote-exec.md §25.4.4，Docker 试用 L7）。
///
/// 缺陷修复：以前原样显示内部的错误链（`cannot reach the executor: link io error: executor exited (exit status: 255):
/// ssh: …`），把 ssh 进程叫成 executor，中文界面里夹着一串英文。现在去掉内部的链，认得的几种写成人话，其余留下最里面
/// 的原文（多是 ssh 的输出）。英文原文照旧在日志里（machines 的状态转移日志）。
pub fn explain(lang: Lang, reason: &str) -> String {
    let inner = innermost(reason);
    let words = |rest: &str| -> Option<(String, String)> {
        let (host, port, why) = {
            let rest = rest.strip_prefix("ssh: connect to host ")?;
            let (host, rest) = rest.split_once(" port ")?;
            let (port, why) = rest.split_once(": ")?;
            (host, port, why)
        };
        Some((
            format!("ssh can't reach {host}:{port} ({why})"),
            format!("ssh 连不上 {host}:{port}（{why}）"),
        ))
    };
    let resolve = |rest: &str| -> Option<(String, String)> {
        let rest = rest.strip_prefix("ssh: Could not resolve hostname ")?;
        let (host, why) = rest.split_once(": ")?;
        Some((
            format!("ssh can't find the host {host} ({why})"),
            format!("ssh 找不到主机 {host}（{why}）"),
        ))
    };
    let closed = |rest: &str| -> Option<(String, String)> {
        let rest = rest.strip_prefix("Connection closed by ")?;
        let (host, port) = rest.split_once(" port ")?;
        let port = port.trim();
        Some((
            format!("{host}:{port} closed the ssh connection"),
            format!("{host}:{port} 关掉了 ssh 连接"),
        ))
    };
    let silent = |rest: &str| -> Option<(String, String)> {
        let ms = rest
            .strip_prefix("no frame from the executor for ")?
            .strip_suffix(" ms")?;
        Some((
            format!("no reply from the executor for {ms} ms"),
            format!("执行器 {ms} ms 没有回应"),
        ))
    };
    let fixed = |rest: &str| -> Option<(String, String)> {
        let zh = match rest {
            "the executor closed the connection" => "执行器关掉了连接",
            "the executor did not answer the handshake" => "执行器没有回应握手",
            _ => return None,
        };
        Some((rest.to_owned(), zh.to_owned()))
    };
    match [words, resolve, closed, silent, fixed]
        .iter()
        .find_map(|known| known(inner))
    {
        Some((en, zh)) => match lang {
            Lang::En => en,
            Lang::Zh => zh,
        },
        None => inner.to_owned(),
    }
}

/// `/mcp` 列表里失败的原因（F98，§25.4.4）：机器没连上的那一种中文写成中文，其余原样。
pub fn mcp_reason(lang: Lang, reason: &str) -> String {
    let machine = reason
        .strip_prefix("Machine \"")
        .and_then(|rest| rest.strip_suffix("\" is not connected."));
    match (lang, machine) {
        (Lang::Zh, Some(name)) => format!("机器 {name} 没有连上。"),
        _ => reason.to_owned(),
    }
}

/// 重连中的进度（F96，remote-exec.md §23.4，写法照模型重试的状态行）：` · retrying in 12s · attempt 3` /
/// ` · 12s后重试 · 第 3 次`；正在连时没有倒计时。
pub fn reconnect_progress(lang: Lang, attempt: u32, left_ms: Option<u64>) -> String {
    let left = left_ms.map(super::retry_status::countdown);
    match (lang, left) {
        (Lang::En, Some(left)) => format!(" · retrying in {left} · attempt {attempt}"),
        (Lang::En, None) => format!(" · attempt {attempt}"),
        (Lang::Zh, Some(left)) => format!(" · {left}后重试 · 第 {attempt} 次"),
        (Lang::Zh, None) => format!(" · 第 {attempt} 次"),
    }
}

/// 列表里重连中的机器的第三行：最近一次的原因（F96）。
pub fn last_error(lang: Lang, reason: &str) -> String {
    let reason = explain(lang, reason);
    match lang {
        Lang::En => format!("Last error: {reason}"),
        Lang::Zh => format!("最近一次的错误：{reason}"),
    }
}

/// 执行器没有 `env` 能力位（F96，§23.5）：重连后不刷新 ssh 会话的 `SSH_AUTH_SOCK`。
pub fn env_not_refreshed(lang: Lang) -> &'static str {
    pick(
        lang,
        "env not refreshed (older executor)",
        "不能刷新环境（执行器较旧）",
    )
}

/// 提示行：enter 随焦点的机器写连接或断开（§21.3）；焦点在等重连的机器上时多“立即重试”（F98，§25.4.1）。
pub fn hint(lang: Lang, focused_live: Option<bool>, retry: bool) -> String {
    let enter = match (lang, focused_live) {
        (_, None) => None,
        (Lang::En, Some(true)) => Some("{enter} to disconnect"),
        (Lang::En, Some(false)) => Some("{enter} to connect"),
        (Lang::Zh, Some(true)) => Some("{enter} 断开"),
        (Lang::Zh, Some(false)) => Some("{enter} 连接"),
    };
    let parts: Vec<&str> = match lang {
        Lang::En => [
            focused_live.map(|_| "{↑/↓} to select"),
            enter,
            retry.then_some("{r} to retry now"),
            focused_live.map(|_| "{x} to remove"),
            Some("{esc} to close"),
        ],
        Lang::Zh => [
            focused_live.map(|_| "{↑/↓} 选择"),
            enter,
            retry.then_some("{r} 立即重试"),
            focused_live.map(|_| "{x} 删除"),
            Some("{esc} 关闭"),
        ],
    }
    .into_iter()
    .flatten()
    .collect();
    parts.join(" · ")
}

pub fn usage(lang: Lang) -> &'static str {
    pick(
        lang,
        "Usage: /machines [add <name> <ssh target> [working directory] | connect <name> | disconnect <name> | remove <name>]",
        "用法：/machines [add <名字> <ssh 目标> [工作目录] | connect <名字> | disconnect <名字> | remove <名字>]",
    )
}

pub fn unavailable(lang: Lang) -> &'static str {
    pick(
        lang,
        "Remote machines aren't available here.",
        "这里不能用远端机器。",
    )
}

pub fn added(lang: Lang, name: &str, ssh: &str) -> String {
    match lang {
        Lang::En => format!("Added {name} (ssh {ssh}). Connecting…"),
        Lang::Zh => format!("已添加 {name}（ssh {ssh}），正在连接…"),
    }
}

pub fn connected(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("Connected {name}."),
        Lang::Zh => format!("已连上 {name}。"),
    }
}

pub fn already_connected(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("{name} is already connected."),
        Lang::Zh => format!("{name} 已经连着。"),
    }
}

pub fn disconnected(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("Disconnected {name}."),
        Lang::Zh => format!("已断开 {name}。"),
    }
}

pub fn not_connected(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("{name} isn't connected."),
        Lang::Zh => format!("{name} 没有连着。"),
    }
}

pub fn removed(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("Removed {name}."),
        Lang::Zh => format!("已删除 {name}。"),
    }
}

/// 操作没做成的原因（§21.3、§21.4）。`name` 是这次操作的机器。
pub fn problem(lang: Lang, name: &str, problem: &MachineProblem) -> String {
    match (lang, problem) {
        (_, MachineProblem::Connection(failure)) => failure_text(lang, name, failure),
        (Lang::En, MachineProblem::Missing(x)) => {
            format!("There's no machine named \"{x}\". Run /machines to see the list.")
        }
        (Lang::Zh, MachineProblem::Missing(x)) => {
            format!("没有叫“{x}”的机器。用 /machines 查看列表。")
        }
        (Lang::En, MachineProblem::Exists(x)) => {
            format!("A machine named \"{x}\" already exists.")
        }
        (Lang::Zh, MachineProblem::Exists(x)) => format!("已经有叫“{x}”的机器了。"),
        (Lang::En, MachineProblem::InvalidName(x)) => format!(
            "\"{x}\" can't be a machine name: use 1-32 letters, digits, '.', '_' or '-', and not \"local\"."
        ),
        (Lang::Zh, MachineProblem::InvalidName(x)) => {
            format!("“{x}”不能当机器名：用 1–32 个字母、数字、.、_、-，不能是 local。")
        }
        (Lang::En, MachineProblem::File(reason)) => {
            format!("Couldn't update the machine list: {reason}")
        }
        (Lang::Zh, MachineProblem::File(reason)) => format!("无法更新机器清单：{reason}"),
    }
}

/// 连接错误的一句话（§21.4）：连接命令的回执与列表里失败的机器共用。
pub fn failure_text(lang: Lang, name: &str, failure: &MachineFailure) -> String {
    match (lang, failure) {
        (Lang::En, MachineFailure::Unreachable(m)) => {
            format!("Couldn't reach {name} over SSH: {}", explain(lang, m))
        }
        (Lang::Zh, MachineFailure::Unreachable(m)) => {
            format!("无法经 SSH 连到 {name}：{}", explain(lang, m))
        }
        (Lang::En, MachineFailure::Unsupported { os, arch }) => {
            format!(
                "{name} runs {os} {arch}; remote machines must be Linux x86_64 / aarch64 or macOS arm64."
            )
        }
        (Lang::Zh, MachineFailure::Unsupported { os, arch }) => {
            format!("{name} 是 {os} {arch}；远端机器只支持 Linux x86_64 / aarch64 与 macOS arm64。")
        }
        (Lang::En, MachineFailure::NoArtifact { version, target }) => format!(
            "There's no zcode {version} build for {target} to upload to {name}, and no compatible executor is installed there."
        ),
        (Lang::Zh, MachineFailure::NoArtifact { version, target }) => format!(
            "没有能上传到 {name} 的 zcode {version}（{target}）产物，那台机器上也没有兼容的执行器。"
        ),
        (
            Lang::En,
            MachineFailure::Incompatible {
                version,
                protocol,
                busy: true,
            },
        ) => format!(
            "The executor on {name} is zcode {version} (protocol {protocol}) and is busy, so it can't be replaced now. Connect again once its work ends, or stop it on {name} with `zcode-rs executor stop` (its background tasks end)."
        ),
        (
            Lang::Zh,
            MachineFailure::Incompatible {
                version,
                protocol,
                busy: true,
            },
        ) => format!(
            "{name} 上的执行器是 zcode {version}（协议 {protocol}），正忙，现在不能换。等它的工作结束后再连，或者在 {name} 上运行 `zcode-rs executor stop` 停掉它（它上面的后台任务会结束）。"
        ),
        (
            Lang::En,
            MachineFailure::Incompatible {
                version, protocol, ..
            },
        ) => format!(
            "The executor on {name} is zcode {version} (protocol {protocol}), which this zcode can't use."
        ),
        (
            Lang::Zh,
            MachineFailure::Incompatible {
                version, protocol, ..
            },
        ) => format!(
            "{name} 上的执行器是 zcode {version}（协议 {protocol}），本机的 zcode 不能用它。"
        ),
        (Lang::En, MachineFailure::Upload(m)) => {
            format!("Uploading the executor to {name} failed: {m}")
        }
        (Lang::Zh, MachineFailure::Upload(m)) => format!("上传执行器到 {name} 失败：{m}"),
        (Lang::En, MachineFailure::Probe(m)) => {
            format!("Couldn't understand {name}'s reply while checking it: {m}")
        }
        (Lang::Zh, MachineFailure::Probe(m)) => format!("检查 {name} 时看不懂它的回复：{m}"),
        // F100（remote-exec.md §26.6）：下载、musl 与 glibc 太旧。
        (
            Lang::En,
            MachineFailure::DownloadFailed {
                version,
                target,
                reason,
            },
        ) => format!(
            "Couldn't download the zcode {version} executor ({target}) to upload to {name}: {reason}. No compatible executor is installed there either."
        ),
        (
            Lang::Zh,
            MachineFailure::DownloadFailed {
                version,
                target,
                reason,
            },
        ) => format!(
            "无法下载要上传到 {name} 的 zcode {version} 执行器（{target}）：{reason}。那台机器上也没有兼容的执行器。"
        ),
        (Lang::En, MachineFailure::Musl) => format!(
            "{name} uses musl libc (for example Alpine), and there's no zcode executor package for musl. Use a glibc-based Linux such as Debian or Ubuntu."
        ),
        (Lang::Zh, MachineFailure::Musl) => format!(
            "{name} 用的是 musl libc（例如 Alpine），没有对应的 zcode 执行器包。请换用基于 glibc 的 Linux（例如 Debian、Ubuntu）。"
        ),
        (Lang::En, MachineFailure::OldGlibc { glibc, needs }) => {
            let has = glibc.as_ref().map_or_else(
                || format!("{name}'s glibc is older"),
                |version| format!("{name} has glibc {version}"),
            );
            format!(
                "The zcode executor can't run on {name}: it needs {needs}, but {has}. Upgrade that machine's system or use a newer Linux."
            )
        }
        (Lang::Zh, MachineFailure::OldGlibc { glibc, needs }) => {
            let has = glibc.as_ref().map_or_else(
                || format!("{name} 的 glibc 更旧"),
                |version| format!("{name} 的 glibc 是 {version}"),
            );
            format!(
                "zcode 执行器在 {name} 上跑不起来：它要 {needs}，{has}。请升级那台机器的系统或换用更新的 Linux。"
            )
        }
        // F103（remote-exec.md §27.7）：本机的版本不在 npm 上，npm 的 latest 说别的协议。
        (
            Lang::En,
            MachineFailure::LatestIncompatible {
                version,
                protocol,
                latest,
                latest_protocol,
                target,
            },
        ) => format!(
            "This zcode ({version}, protocol {protocol}) isn't on npm, and the latest executor on npm, zcode {latest}, speaks protocol {latest_protocol}, so there's no executor for {name}. Connect again after a new version is published, or put a zcode {version} executor at {} on this computer.",
            cached_executor(version, target)
        ),
        (
            Lang::Zh,
            MachineFailure::LatestIncompatible {
                version,
                protocol,
                latest,
                latest_protocol,
                target,
            },
        ) => format!(
            "本机的 zcode {version}（协议 {protocol}）不在 npm 上，npm 上最新的执行器 zcode {latest} 是协议 {latest_protocol}，{name} 没有能用的执行器。请在发布新版之后再连，或者把 zcode {version} 的执行器放到本机的 {}。",
            cached_executor(version, target)
        ),
        // F107（remote-exec.md §28.6）：这个平台没有包（Intel Mac）、上传的执行器被系统杀掉（签名无效）。
        (Lang::En, MachineFailure::NoPackage { package }) => format!(
            "There's no zcode executor package for {name} (it would be {package}), and no compatible executor is installed there. Install zcode on that machine with `npm i -g @mbearo/zcode`, then connect again."
        ),
        (Lang::Zh, MachineFailure::NoPackage { package }) => format!(
            "没有 {name} 这种平台的 zcode 执行器包（应该是 {package}），那台机器上也没有兼容的执行器。请在那台机器上用 `npm i -g @mbearo/zcode` 装好 zcode，然后再连。"
        ),
        (Lang::En, MachineFailure::Unsigned) => format!(
            "The uploaded zcode executor was killed on {name}: macOS refuses to run it because its signature is not valid. Install zcode on that machine with `npm i -g @mbearo/zcode`, or sign the file there with `codesign -s -`."
        ),
        (Lang::Zh, MachineFailure::Unsigned) => format!(
            "上传到 {name} 的 zcode 执行器被系统杀掉了：macOS 认为它的签名无效，不让它运行。请在那台机器上用 `npm i -g @mbearo/zcode` 装好 zcode，或者在那台机器上用 `codesign -s -` 给它签名。"
        ),
        (Lang::En, MachineFailure::Refused(m)) => {
            format!("The executor on {name} refused the connection: {m}")
        }
        (Lang::Zh, MachineFailure::Refused(m)) => format!("{name} 上的执行器拒绝了连接：{m}"),
        (Lang::En, MachineFailure::Lost(m)) => {
            format!("Lost the connection to {name}: {}", explain(lang, m))
        }
        (Lang::Zh, MachineFailure::Lost(m)) => {
            format!("与 {name} 的连接断了：{}", explain(lang, m))
        }
    }
}

/// 本机缓存里放执行器的位置（F103，remote-exec.md §27.7）：`~/.zcode/rust-cli/exec/dist/<版本>/<target>/zcode-rs`。
fn cached_executor(version: &str, target: &str) -> String {
    use rust_cli_base::names::{DOT_DIR, EXECUTOR_HOME, UNIX_EXECUTABLE};
    format!("~/{DOT_DIR}/{EXECUTOR_HOME}/dist/{version}/{target}/{UNIX_EXECUTABLE}")
}

/// 询问框标题后面的机器（§21.6）：`runs on dev-a` / `在 dev-a 上执行`（前缀、机器名、后缀，机器名用强调色）。
pub fn runs_on(lang: Lang) -> (&'static str, &'static str) {
    pick2(lang, ("runs on ", ""), ("在 ", " 上执行"))
}

/// Shell 详情的机器一行（§21.6）。
pub fn machine_field(lang: Lang) -> &'static str {
    pick(lang, "Machine: ", "机器：")
}

fn pick2(
    lang: Lang,
    en: (&'static str, &'static str),
    zh: (&'static str, &'static str),
) -> (&'static str, &'static str) {
    match lang {
        Lang::En => en,
        Lang::Zh => zh,
    }
}

#[cfg(test)]
#[path = "i18n_machines_tests.rs"]
mod tests;
