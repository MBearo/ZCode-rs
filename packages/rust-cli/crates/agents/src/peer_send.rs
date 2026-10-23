//! ListAgents 的段落与 SendMessage 的同伴寻址（docs/specs/rust-cli/cross-session.md §1.3、§1.4、§2.5）。
//!
//! 文字已套产品名替换表。
//! 发送经 kernel-api 的 [`PeerMessaging`] 端口，本 crate 不做 IO。

use rust_cli_kernel_api::peers::{
    ModeClass, PeerAddress, PeerOutbound, PeerRecord, PeerSendError, envelope, name_key,
    shortest_refs, split_reference,
};
use rust_cli_kernel_api::tool::ToolUseContext;
use rust_cli_prompts::peers as texts;
use rust_cli_prompts::texts::render;

use crate::Core;

/// 每段最多几行。
const ROWS: usize = 100;

/// 时长只取最大的单位：不到 1 分钟是秒。
pub(crate) fn ago(ms: u64) -> String {
    if ms < 60_000 {
        return format!("{}s", ms / 1000);
    }
    let mut days = ms / 86_400_000;
    let mut hours = (ms % 86_400_000) / 3_600_000;
    let mut minutes = (ms % 3_600_000) / 60_000;
    let seconds = ((ms % 60_000) as f64 / 1000.0).round() as u64;
    if seconds == 60 {
        minutes += 1;
    }
    if minutes == 60 {
        minutes = 0;
        hours += 1;
    }
    if hours == 24 {
        hours = 0;
        days += 1;
    }
    if days > 0 {
        format!("{days}d")
    } else if hours > 0 {
        format!("{hours}h")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{seconds}s")
    }
}

/// 一段：`<标题> (<n>):` 与每行。
fn section(title: &str, rows: Vec<String>) -> String {
    let total = rows.len();
    let mut lines: Vec<String> = rows.into_iter().take(ROWS).collect();
    if total > ROWS {
        lines.push(format!("  (… {} more not shown)", total - ROWS));
    }
    format!("{title} ({total}):\n{}", lines.join("\n"))
}

/// 一行：两个空格开头、各项用 `  ·  ` 连接。
fn row(items: &[&str]) -> String {
    format!("  {}", items.join("  ·  "))
}

/// 同伴与自己的 `[ref]`：12 位串的最短唯一前缀，自己固定 6 位。
fn peer_refs(peers: &[PeerRecord], own: Option<&PeerAddress>) -> Vec<String> {
    let mut all: Vec<String> = peers.iter().map(|peer| peer.address.reference()).collect();
    if let Some(own) = own {
        all.push(own.reference());
    }
    let mut refs = shortest_refs(&all);
    refs.truncate(peers.len());
    refs
}

/// ListAgents 的全文。
pub(crate) async fn listing(core: &Core, context: &ToolUseContext) -> String {
    let now = core.ports.clock.now_ms();
    let mut sections = Vec::new();
    let agents = core.live.spawned_by(&context.session);
    if !agents.is_empty() {
        let rows = agents
            .iter()
            .map(|agent| {
                let started = format!(
                    "started {} ago",
                    ago(now.saturating_sub(agent.shown.started_ms))
                );
                row(&[
                    agent.agent.as_str(),
                    agent.definition.agent_type.as_str(),
                    agent.status(),
                    &started,
                ])
            })
            .collect();
        sections.push(section("Subagents", rows));
    }
    let identity = core.ports.peers.identity().await;
    let peers = core.ports.peers.peers().await;
    if !peers.is_empty() {
        let refs = peer_refs(&peers, identity.as_ref().map(|own| &own.address));
        let rows = peers
            .iter()
            .zip(refs)
            .map(|(peer, reference)| {
                let token = format!("{} [{reference}]", peer.name);
                let started = format!(
                    "started {} ago",
                    ago(now.saturating_sub(peer.started_at_ms))
                );
                let mut items = vec![token.as_str(), peer.kind.as_str()];
                if let Some(status) = peer.status {
                    items.push(status.as_str());
                }
                items.push(&started);
                row(&items)
            })
            .collect();
        sections.push(section("Peer sessions", rows));
    }
    let own = identity.map(|own| {
        let token = format!("{} [{}]", own.name, &own.address.reference()[..6]);
        render(texts::list_self(), &[("self", token.as_str())])
    });
    match (own, sections.is_empty()) {
        (Some(own), true) => format!("{own}\n\n{}", texts::list_none()),
        (None, true) => "No reachable agents.".to_owned(),
        (Some(own), false) => format!("{own}\n\n{}", sections.join("\n\n")),
        (None, false) => sections.join("\n\n"),
    }
}

/// 解析出的收件方。
enum Target {
    One(PeerRecord),
    Address(PeerAddress),
    Ambiguous(Vec<(PeerRecord, String)>),
    None,
}

async fn resolve(core: &Core, to: &str) -> Target {
    let peers = core.ports.peers.peers().await;
    if let Some(address) = PeerAddress::parse(to.trim()) {
        return match peers.into_iter().find(|peer| peer.address == address) {
            Some(peer) => Target::One(peer),
            None => Target::Address(address),
        };
    }
    let (name, reference) = match split_reference(to) {
        Some((name, reference)) => (name, Some(reference)),
        None => (to.trim(), None),
    };
    let key = name_key(name);
    let mut matched: Vec<PeerRecord> = peers
        .into_iter()
        .filter(|peer| name_key(&peer.name) == key)
        .filter(|peer| reference.is_none_or(|prefix| peer.address.reference().starts_with(prefix)))
        .collect();
    match matched.len() {
        0 => Target::None,
        1 => Target::One(matched.remove(0)),
        _ => {
            let own = core.ports.peers.identity().await;
            let refs = peer_refs(&matched, own.as_ref().map(|own| &own.address));
            Target::Ambiguous(matched.into_iter().zip(refs).collect())
        }
    }
}

/// 发给同伴会话；找不到同伴时为 None（调用方按“没有这个 agent”回答）。返回给模型的文字（结果形状的已登记差异：
/// 纯文本 `message`，agents.md §9）。
pub(crate) async fn send(
    core: &Core,
    to: &str,
    summary: Option<&str>,
    message: &str,
    context: &ToolUseContext,
) -> String {
    let unknown = || render(texts::send_unknown(), &[("to", to)]);
    let Some(own) = core.ports.peers.identity().await else {
        return unknown();
    };
    let (address, name) = match resolve(core, to).await {
        Target::One(peer) => (peer.address, peer.name),
        Target::Address(address) => {
            let name = address.as_str().to_owned();
            (address, name)
        }
        Target::Ambiguous(candidates) => return ambiguous(to, &candidates, core),
        Target::None => return unknown(),
    };
    // 发给自己：本会话不在同伴里（列表也不列自己）。
    if address == own.address {
        return unknown();
    }
    let mode = ModeClass::of(context.mode);
    let outbound = PeerOutbound {
        to: address,
        msg_id: core.ports.ids.command().to_string(),
        recipient_name: name.clone(),
        envelope: envelope(&own.address, Some(&own.name), mode, message),
    };
    match core.ports.peers.send(outbound).await {
        Ok(()) => {
            // summary 缺省时取 message 的前 50 字。
            let summary = summary
                .filter(|summary| !summary.is_empty())
                .map_or_else(|| first_chars(message, 50), str::to_owned);
            let mut text = render(
                texts::send_ok(),
                &[("summary", summary.as_str()), ("name", name.as_str())],
            );
            // 子代理发出的消息用会话的地址（收件箱已绑定时的写法）。
            if context.agent.is_some()
                && let Some(stripped) = text.strip_suffix(')')
            {
                text = format!(
                    "{stripped}; sent under this session's address: any reply will be delivered to this session's main conversation, not to this agent)"
                );
            }
            text
        }
        Err(PeerSendError::Gone | PeerSendError::Refused { .. } | PeerSendError::Failed { .. }) => {
            render(texts::send_failed(), &[("name", name.as_str())])
        }
    }
}

fn first_chars(text: &str, count: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= count {
        return text.to_owned();
    }
    let mut out: String = chars[..count.saturating_sub(1)].iter().collect();
    out.push('…');
    out
}

/// 几个同伴同名（`<n> agents are named '<to>'. Re-send with the ref of the one you mean:`）。
fn ambiguous(to: &str, candidates: &[(PeerRecord, String)], core: &Core) -> String {
    let now = core.ports.clock.now_ms();
    let rows: Vec<String> = candidates
        .iter()
        .map(|(peer, reference)| {
            format!(
                "  {} [{reference}] — ZCode session, on this machine, active {} ago",
                peer.name,
                ago(now.saturating_sub(peer.started_at_ms))
            )
        })
        .collect();
    format!(
        "{} agents are named '{to}'. Re-send with the ref of the one you mean:\n{}",
        candidates.len(),
        rows.join("\n")
    )
}

#[cfg(test)]
#[path = "peer_send_tests.rs"]
mod tests;
