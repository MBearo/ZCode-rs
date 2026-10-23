//! `/mcp reconnect|enable|disable [<server>|all]`（docs/specs/rust-cli/tui-manage.md §6.2）：先按打开时的服务器
//! 快照判断（前置检查），要动的交给 runner（先写配置、再改连接），回执到了再按固定的句子提示。
//! 这里没有业务状态：服务器的状态只来自内核的查询与回执。
use rust_cli_kernel_api::manage::{McpAction, McpServerInfo, McpServerState};

use super::i18n::Lang;

/// 前置检查的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// 不用动，直接提示。
    Say(String),
    Run(Run),
}

/// 要做的动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub action: McpAction,
    /// 点名的一个服务器；None 是 all。
    pub one: Option<String>,
    pub names: Vec<String>,
}

impl Run {
    /// enable / disable 要写配置的开关；reconnect 不写。
    pub fn persist(&self) -> Option<bool> {
        match self.action {
            McpAction::Reconnect => None,
            McpAction::Enable => Some(true),
            McpAction::Disable => Some(false),
        }
    }
}

/// 状态词。
pub fn state_word(state: McpServerState, lang: Lang) -> &'static str {
    match (lang, state) {
        (Lang::En, McpServerState::Connected) => "connected",
        (Lang::En, McpServerState::Pending) => "connecting",
        (Lang::En, McpServerState::Disabled) => "disabled",
        (Lang::En, McpServerState::Failed) => "not connected",
        (Lang::Zh, McpServerState::Connected) => "已连接",
        (Lang::Zh, McpServerState::Pending) => "连接中",
        (Lang::Zh, McpServerState::Disabled) => "已停用",
        (Lang::Zh, McpServerState::Failed) => "未连接",
    }
}

fn count(servers: &[McpServerInfo], state: McpServerState) -> usize {
    servers
        .iter()
        .filter(|server| server.state == state)
        .count()
}

/// 前置检查。`args` 是 `/mcp` 之后的文字（不为空）。
pub fn plan(args: &str, servers: &[McpServerInfo], lang: Lang) -> Plan {
    let zh = lang == Lang::Zh;
    let mut words = args.split_whitespace();
    let word = words.next().unwrap_or_default().to_lowercase();
    let target = words.next().unwrap_or("all").to_owned();
    let action = match word.as_str() {
        "reconnect" => McpAction::Reconnect,
        "enable" => McpAction::Enable,
        "disable" => McpAction::Disable,
        _ => {
            return Plan::Say(if zh {
                format!("“{word}”不是 /mcp 的动作。可用 reconnect、enable 或 disable。")
            } else {
                format!(
                    "\"{word}\" isn't a recognized /mcp action. Try reconnect, enable, or disable."
                )
            });
        }
    };
    if servers.is_empty() {
        return Plan::Say(
            if zh {
                "没有配置 MCP 服务器。"
            } else {
                "No MCP servers are configured."
            }
            .to_owned(),
        );
    }
    let all = target == "all";
    let chosen: Vec<&McpServerInfo> = servers
        .iter()
        .filter(|server| all || server.name == target)
        .collect();
    let Some(first) = chosen.first().copied() else {
        return Plan::Say(if zh {
            format!("没有名为“{target}”的 MCP 服务器。")
        } else {
            format!("There's no MCP server named \"{target}\".")
        });
    };
    let run = |names: Vec<String>| {
        Plan::Run(Run {
            action,
            one: (!all).then(|| target.clone()),
            names,
        })
    };
    let state = first.state;
    match (action, all) {
        (McpAction::Reconnect, false) => match state {
            McpServerState::Disabled => Plan::Say(if zh {
                format!("“{target}”已停用。用 `/mcp enable {target}` 重新启用。")
            } else {
                format!("\"{target}\" is disabled. Run `/mcp enable {target}` to bring it back.")
            }),
            McpServerState::Pending => Plan::Say(if zh {
                format!("“{target}”已经在重连——服务器一直失败时，重试可能要几分钟。")
            } else {
                format!(
                    "\"{target}\" is already reconnecting — retries can take a few minutes when a server keeps failing."
                )
            }),
            McpServerState::Connected | McpServerState::Failed => run(vec![target.clone()]),
        },
        (McpAction::Reconnect, true) => {
            let failed: Vec<String> = servers
                .iter()
                .filter(|server| server.state == McpServerState::Failed)
                .map(|server| server.name.clone())
                .collect();
            if !failed.is_empty() {
                return run(failed);
            }
            let disabled = count(servers, McpServerState::Disabled);
            Plan::Say(match (zh, disabled) {
                (false, 0) => {
                    "All enabled MCP servers are already connected or connecting.".to_owned()
                }
                (true, 0) => "已启用的 MCP 服务器都已连接或正在连接。".to_owned(),
                (false, n) => format!(
                    "{n} MCP server(s) are disabled. Run `/mcp enable all` to bring them back."
                ),
                (true, n) => format!("{n} 个 MCP 服务器已停用。用 `/mcp enable all` 重新启用。"),
            })
        }
        (McpAction::Enable | McpAction::Disable, _) => {
            let enable = action == McpAction::Enable;
            let targets: Vec<String> = chosen
                .iter()
                .filter(|server| (server.state == McpServerState::Disabled) == enable)
                .map(|server| server.name.clone())
                .collect();
            if !targets.is_empty() {
                return run(targets);
            }
            let failed = chosen
                .iter()
                .filter(|server| server.state == McpServerState::Failed)
                .count();
            Plan::Say(already(enable, all, &target, failed, lang))
        }
    }
}

fn already(enable: bool, all: bool, target: &str, failed: usize, lang: Lang) -> String {
    let zh = lang == Lang::Zh;
    let verb = match (zh, enable) {
        (false, true) => "enabled",
        (false, false) => "disabled",
        (true, true) => "启用",
        (true, false) => "停用",
    };
    if enable && failed > 0 {
        return match (zh, all) {
            (false, true) => format!(
                "All MCP servers are already enabled, but {failed} {} connected — reply `/mcp reconnect all` here to retry.",
                if failed == 1 { "isn't" } else { "aren't" }
            ),
            (true, true) => format!(
                "MCP 服务器都已启用，但有 {failed} 个没有连上——用 `/mcp reconnect all` 重试。"
            ),
            (false, false) => format!(
                "\"{target}\" is already enabled but not connected. Run `/mcp reconnect {target}` to retry."
            ),
            (true, false) => {
                format!("“{target}”已启用但没有连上。用 `/mcp reconnect {target}` 重试。")
            }
        };
    }
    match (zh, all) {
        (false, true) => format!("All MCP servers are already {verb}."),
        (true, true) => format!("MCP 服务器都已{verb}。"),
        (false, false) => format!("\"{target}\" is already {verb}."),
        (true, false) => format!("“{target}”已经{verb}。"),
    }
}

/// 回执到了之后的提示（结果句）。`failed` 是写配置失败的服务器与原因；`infos` 是改了连接的服务器
/// 改动后的状态。
pub fn result(
    run: &Run,
    infos: &[McpServerInfo],
    failed: &[(String, String)],
    lang: Lang,
) -> String {
    let zh = lang == Lang::Zh;
    let verb = match (zh, run.action) {
        (false, McpAction::Reconnect) => "reconnect",
        (false, McpAction::Enable) => "enable",
        (false, McpAction::Disable) => "disable",
        (true, McpAction::Reconnect) => "重连",
        (true, McpAction::Enable) => "启用",
        (true, McpAction::Disable) => "停用",
    };
    if let Some(name) = &run.one {
        if let Some((_, reason)) = failed.first() {
            return if zh {
                format!("无法{verb}“{name}”——{reason}。")
            } else {
                format!("Couldn't {verb} \"{name}\" — {reason}.")
            };
        }
        let state = infos
            .first()
            .map_or(McpServerState::Failed, |info| info.state);
        let word = state_word(state, lang);
        return match (run.action, state, zh) {
            (McpAction::Reconnect, McpServerState::Connected, false) => {
                format!("Reconnected \"{name}\".")
            }
            (McpAction::Reconnect, McpServerState::Connected, true) => format!("已重连“{name}”。"),
            (McpAction::Reconnect, _, false) => format!("Couldn't reconnect \"{name}\" ({word})."),
            (McpAction::Reconnect, _, true) => format!("无法重连“{name}”（{word}）。"),
            (McpAction::Disable, _, false) => format!("Disabled \"{name}\"."),
            (McpAction::Disable, _, true) => format!("已停用“{name}”。"),
            (McpAction::Enable, McpServerState::Connected, false) => format!("Enabled \"{name}\"."),
            (McpAction::Enable, McpServerState::Connected, true) => format!("已启用“{name}”。"),
            (McpAction::Enable, McpServerState::Failed, false) => {
                format!("Enabled \"{name}\", but it isn't connected yet.")
            }
            (McpAction::Enable, _, false) => {
                format!("Enabled \"{name}\", but it isn't connected yet ({word}).")
            }
            (McpAction::Enable, _, true) => format!("已启用“{name}”，但还没有连上（{word}）。"),
        };
    }
    let connected = count(infos, McpServerState::Connected);
    let changed = infos.len();
    let skipped = failed.len();
    let mut text = match (run.action, zh) {
        (McpAction::Reconnect, false) => {
            return format!(
                "Reconnected {connected} of {} MCP server(s).",
                run.names.len()
            );
        }
        (McpAction::Reconnect, true) => {
            return format!(
                "已重连 {} 个 MCP 服务器里的 {connected} 个。",
                run.names.len()
            );
        }
        (McpAction::Enable, false) => format!("Enabled {changed} MCP server(s)"),
        (McpAction::Enable, true) => format!("已启用 {changed} 个 MCP 服务器"),
        (McpAction::Disable, false) => format!("Disabled {changed} MCP server(s)"),
        (McpAction::Disable, true) => format!("已停用 {changed} 个 MCP 服务器"),
    };
    if run.action == McpAction::Enable && connected < changed {
        text.push_str(&if zh {
            format!("（{} 个已启用但还没有连上）", changed - connected)
        } else {
            format!(" ({} enabled but not yet connected)", changed - connected)
        });
    }
    if skipped > 0 {
        text.push_str(&if zh {
            format!("（{skipped} 个改不了）")
        } else {
            format!(" ({skipped} couldn't be changed)")
        });
    }
    text.push_str(if zh { "。" } else { "." });
    text
}

#[cfg(test)]
#[path = "mcp_control_tests.rs"]
mod tests;
