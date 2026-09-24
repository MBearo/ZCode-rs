//! Run side of `@plugin` references (Node `injectPluginReferenceReminderFromTurn`,
//! spec rust-m10-plugins §3.9): the turn's references resolved against the
//! frozen catalog and the live capabilities, committed as a model-only notice.
use super::context::RunContext;
use crate::contract::{Event, EventSink, ToolPort};
use crate::domain::plugin_reference::{self, Live, LiveServer, LiveSkill, LiveSubagent};
use crate::domain::skills::SkillCatalog;
use anyhow::Result;
use serde_json::Value;
use std::collections::BTreeSet;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// Before the turn's first model request: nothing without references; a
/// resolution failure skips the reminder (fail open), never the turn.
pub(super) async fn inject(
    tools: &dyn ToolPort,
    history: &mut RunContext,
    (skills, definitions): (&SkillCatalog, &[Value]),
    sink: &EventSink,
    cancel: &CancellationToken,
) -> Result<()> {
    let references = std::mem::take(&mut history.plugin_references);
    if references.is_empty() {
        return Ok(());
    }
    let (reply, receipt) = oneshot::channel();
    sink.send(Event::PluginCatalog { reply }).await?;
    let catalog = tokio::select! {biased;
        _ = cancel.cancelled() => anyhow::bail!("Cancelled"),
        catalog = receipt => match catalog {
            Ok(catalog) => catalog,
            Err(_) => return Ok(()),
        },
    };
    let visible: BTreeSet<&str> = definitions
        .iter()
        .filter_map(|d| d["function"]["name"].as_str())
        .collect();
    let inventory = tools.mcp_inventory(&sink.session_id);
    let plugin_skills: Vec<(String, &str, &str)> = skills
        .skills
        .iter()
        .filter_map(|s| {
            let (plugin, root) = (s.plugin_name.as_deref()?, s.plugin_root.as_deref()?);
            Some((s.qualified_name(), plugin, root))
        })
        .collect();
    let profiles = tools.agent_profiles(cancel).await.unwrap_or_default();
    let live = Live {
        skills: plugin_skills
            .iter()
            .map(|(qualified, plugin, root)| LiveSkill {
                qualified,
                plugin,
                root,
            })
            .collect(),
        servers: inventory
            .iter()
            .map(|(name, bound)| LiveServer {
                name,
                visible_tools: bound
                    .iter()
                    .filter(|t| visible.contains(t.as_str()))
                    .count(),
            })
            .collect(),
        subagents: profiles
            .iter()
            .filter_map(|p| {
                Some(LiveSubagent {
                    name: &p.name,
                    path: p.path.as_deref()?,
                })
            })
            .collect(),
    };
    let Some(body) = plugin_reference::reminder(&references, &catalog, &live) else {
        return Ok(());
    };
    let message = crate::domain::plan_mode::reminder_message(&body);
    let (committed, receipt) = oneshot::channel();
    sink.send(Event::ModelOnlyNotice {
        source: "plugin_reference",
        body,
        message: message.clone(),
        committed,
    })
    .await?;
    // owner 提交并持久化后才进入模型请求，冷恢复与热会话的历史前缀一致。
    super::agent_loop::durable(receipt, cancel).await?;
    history.push(message);
    Ok(())
}
