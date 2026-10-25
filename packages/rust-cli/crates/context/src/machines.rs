//! 机器清单提醒（docs/specs/rust-cli/remote-exec.md §20.5.3）：连上、断开、重连时告诉模型有哪些远端机器。
//!
//! 只在有变化时发：最近一次压缩边界之后的上一条提醒存着快照（`data.machines`），与注册表现在的快照比。没有远端机器、
//! 也没发过时什么都不发（没有机器时完全不变）。不改 system prompt。

use rust_cli_kernel_api::env_registry::MachineSet;
use rust_cli_kernel_api::message::Message;
use rust_cli_prompts::texts::{self, render};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::render::attachments;

pub(crate) const SOURCE: &str = "remote_machines";

/// 快照里的一台机器（字段名与落库记录里的相同）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    name: String,
    os: String,
    arch: String,
    shell: String,
    working_directory: String,
}

fn snapshot(set: &MachineSet) -> Vec<Entry> {
    set.machines
        .iter()
        .map(|machine| Entry {
            name: machine.name.clone(),
            os: machine.facts.os.clone(),
            arch: machine.facts.arch.clone(),
            shell: machine.facts.shell.clone(),
            working_directory: machine.workspace.root.as_str().to_owned(),
        })
        .collect()
}

/// 执行器报告的系统名写成常见的写法。
fn system(os: &str) -> &str {
    match os {
        "linux" => "Linux",
        "macos" => "macOS",
        "windows" => "Windows",
        other => other,
    }
}

fn line(entry: &Entry) -> String {
    let shell = if entry.shell.is_empty() {
        "unknown"
    } else {
        &entry.shell
    };
    render(
        texts::machine_line(),
        &[
            ("name", &entry.name),
            ("system", &format!("{} {}", system(&entry.os), entry.arch)),
            ("shell", shell),
            ("cwd", &entry.working_directory),
        ],
    )
}

/// 这一步的机器清单提醒：`(text, data)`，text 未包 `<system-reminder>`。`history` 是最近一次压缩边界之后的部分；
/// `default` 是这次运行的缺省机器（本机为 None）。
///
/// 缺陷修复（F98，remote-exec.md §25.2.1）：以前只有一种写法（“不写 `machine` 就在本机”），缺省机器在 dev-a 的子代理
/// 读到的提醒与它的工具说明（`omit to use the default machine "dev-a"`）、实际的路由都不一致。现在按缺省机器选文字；
/// 快照（`data`）不变：子代理的缺省机器在它的一生里不变，不必存。
pub(crate) fn announce(
    history: &[Message],
    set: &MachineSet,
    default: Option<&str>,
) -> Option<(String, Value)> {
    let current = snapshot(set);
    let latest: Option<Vec<Entry>> = attachments(history, SOURCE)
        .next_back()
        .and_then(|attachment| serde_json::from_value(attachment.data["machines"].clone()).ok());
    let previous = match latest {
        None if current.is_empty() => return None,
        None => Vec::new(),
        Some(latest) if latest == current => return None,
        Some(latest) => latest,
    };
    let mut lines: Vec<String> = current.iter().map(line).collect();
    lines.extend(
        previous
            .iter()
            .filter(|entry| !current.iter().any(|now| now.name == entry.name))
            .map(|entry| render(texts::machine_gone(), &[("name", &entry.name)])),
    );
    let template = match (current.is_empty(), default) {
        (true, None) => texts::machines_none(),
        (false, None) => texts::machines(),
        (true, Some(_)) => texts::machines_none_default(),
        (false, Some(_)) => texts::machines_default(),
    };
    let lines = lines.join("\n");
    let mut vars = vec![("lines", lines.as_str())];
    vars.extend(default.map(|name| ("name", name)));
    Some((render(template, &vars), json!({"machines": current})))
}

#[cfg(test)]
#[path = "machines_tests.rs"]
mod tests;
