use super::Engine;
use crate::contract::Event;
use anyhow::{Result, ensure};
use serde_json::json;
impl Engine {
    pub(super) async fn context_event(&mut self, id: &str, event: Event) -> Result<()> {
        let turn = self.active[id].turn_id.clone();
        let session = self.sessions.get_mut(id).unwrap();
        let mut deltas = vec![];
        let mut receipt = None;
        match event {
            Event::SkillsInitialized { catalog, reply } => {
                // catalog RPC 可能先于发现事件完成；统一返回 owner 已提交的快照，避免两份能力目录。
                if session.skills.is_none() {
                    session.skills = Some(catalog);
                    self.persist(id, None).await?;
                }
                let _ = reply.send(self.sessions[id].skills.clone().unwrap());
                return Ok(());
            }
            Event::PromptInitialized {
                snapshot,
                skills,
                committed,
            } => {
                // 首次环境快照必须先提交；数据库失败时不得继续向模型发送未确定的请求上下文。
                ensure!(
                    session.prompt_snapshot.is_none(),
                    "Prompt snapshot already initialized"
                );
                session.prompt_snapshot = Some(*snapshot);
                if session.skills.is_none() {
                    session.skills = Some(skills);
                }
                self.persist(id, None).await?;
                let _ = committed.send(self.sessions[id].skills.clone().unwrap());
                return Ok(());
            }
            Event::ContextUsage(usage) => session.usage["contextWindow"] = usage,
            Event::CompactStarted {
                id,
                manual,
                tokens,
                committed,
            } => {
                let mut row = session.row("timelineMarker", &turn, &id, self.clock.now());
                row["lane"] = "assistantWork".into();
                row["marker"] = json!({"type":"compact","origin":if manual {"manual"} else {"auto"},"status":"running","tokensBefore":tokens});
                session.rows.push(row.clone());
                deltas.push(json!({"op":"row.appended","row":row}));
                receipt = Some(committed);
            }
            Event::CompactDone {
                id,
                context,
                tokens,
                usage,
                committed,
            } => {
                ensure!(
                    context.offset >= session.context.offset
                        && context.offset <= session.messages.len(),
                    "Invalid context boundary"
                );
                let changed = context.offset > session.context.offset;
                session.context = context;
                session.context_tokens = Some(tokens);
                if session.usage["contextWindow"].is_object() {
                    session.usage["contextWindow"]["usedTokens"] = tokens.into();
                }
                for (from, to) in [
                    ("prompt_tokens", "inputTokens"),
                    ("completion_tokens", "outputTokens"),
                ] {
                    session.usage["cumulative"][to] = session.usage["cumulative"][to]
                        .as_u64()
                        .unwrap_or(0)
                        .saturating_add(usage[from].as_u64().unwrap_or(0))
                        .into();
                }
                let row = session
                    .rows
                    .iter_mut()
                    .find(|r| r["entityId"] == id)
                    .ok_or_else(|| anyhow::anyhow!("Compaction marker missing"))?;
                row["marker"]["status"] = if changed { "success" } else { "noop" }.into();
                row["marker"]["tokensAfter"] = tokens.into();
                deltas.push(json!({"op":"row.upserted","row":row}));
                receipt = Some(committed);
                session.revision += 1;
            }
            _ => unreachable!(),
        }
        self.publish(id, deltas)?;
        if let Some(receipt) = receipt {
            self.persist(id, None).await?;
            let _ = receipt.send(());
        }
        Ok(())
    }
}
