//! 远端机器的文字（docs/specs/rust-cli/remote-exec.md §21.3、§21.4、§21.5）：`/machines` 的列表与回执、连接错误的说明、
//! 状态行的状态词。中英文同一张表（照 `i18n_*` 的写法）。
use super::i18n::Lang;
use crate::machines::{MachineFailure, MachineProblem, MachineStatus};

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

/// 状态行里名字后面的词；就绪时没有（写延迟）。
pub fn short_word(lang: Lang, status: &MachineStatus) -> Option<&'static str> {
    Some(match status {
        MachineStatus::Ready => return None,
        MachineStatus::Connecting => pick(lang, "connecting", "连接中"),
        MachineStatus::Reconnecting { .. } => pick(lang, "reconnecting", "重连中"),
        MachineStatus::Failed(_) => pick(lang, "failed", "连接失败"),
        MachineStatus::Disconnected => pick(lang, "disconnected", "未连接"),
    })
}

/// 提示行：enter 随焦点的机器写连接或断开（§21.3）。
pub fn hint(lang: Lang, focused_live: Option<bool>) -> String {
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
            focused_live.map(|_| "{x} to remove"),
            Some("{esc} to close"),
        ],
        Lang::Zh => [
            focused_live.map(|_| "{↑/↓} 选择"),
            enter,
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
            format!("Couldn't reach {name} over SSH: {m}")
        }
        (Lang::Zh, MachineFailure::Unreachable(m)) => format!("无法经 SSH 连到 {name}：{m}"),
        (Lang::En, MachineFailure::Unsupported { os, arch }) => {
            format!("{name} runs {os} {arch}; remote machines must be Linux x86_64 or aarch64.")
        }
        (Lang::Zh, MachineFailure::Unsupported { os, arch }) => {
            format!("{name} 是 {os} {arch}；远端机器只支持 Linux x86_64 与 aarch64。")
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
        (Lang::En, MachineFailure::Refused(m)) => {
            format!("The executor on {name} refused the connection: {m}")
        }
        (Lang::Zh, MachineFailure::Refused(m)) => format!("{name} 上的执行器拒绝了连接：{m}"),
        (Lang::En, MachineFailure::Lost(m)) => format!("Lost the connection to {name}: {m}"),
        (Lang::Zh, MachineFailure::Lost(m)) => format!("与 {name} 的连接断了：{m}"),
    }
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
