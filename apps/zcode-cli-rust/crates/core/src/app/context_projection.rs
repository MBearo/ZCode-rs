use super::Engine;
use crate::contract::Event;
use anyhow::{Result, ensure};
use serde_json::json;
impl Engine {
    pub(super) async fn context_event(&mut self, id: &str, event: Event) -> Result<()> {
        // 压缩事件的 `id` 是标记 id，会遮蔽会话 id。
        let session_id = id;
        self.node_compact(id, &event);
        let turn = self.active[id].turn_id.clone();
        let session = self.sessions.get_mut(id).unwrap();
        let mut deltas = vec![];
        let receipt;
        let mut compact_complete = None;
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
                session.stored_env = None;
                if session.skills.is_none() {
                    session.skills = Some(skills);
                }
                let now = self.clock.now();
                let session = self.sessions.get_mut(id).unwrap();
                session.node_prompt_snapshot(now);
                self.persist(id, None).await?;
                let _ = committed.send(self.sessions[id].skills.clone().unwrap());
                return Ok(());
            }
            Event::RequestContext { window, breakdown } => {
                if let Some(active) = self.active.get_mut(id) {
                    active.request = Some(super::usage_state::StepRequest { window, breakdown });
                }
                return Ok(());
            }
            Event::CompactStarted {
                id,
                manual,
                tokens,
                prefix,
                committed,
                ..
            } => {
                // Node 的压缩前 token 包含请求前缀（system prompt、上下文与技能提醒）。
                let tokens = tokens + prefix;
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
                prefix,
                usage,
                body,
                reminders,
                committed,
                ..
            } => {
                ensure!(
                    context.offset >= session.context.offset
                        && context.offset <= session.messages.len(),
                    "Invalid context boundary"
                );
                let changed = context.offset > session.context.offset;
                // Node：自动与反应式压缩成功后清零连续失败次数（手动压缩不影响）。
                let automatic = session
                    .rows
                    .iter()
                    .any(|r| r["entityId"] == id && r["marker"]["origin"] == "auto");
                if changed && automatic {
                    session.runtime.compact_failures = 0;
                }
                session.context = context;
                // 被摘要覆盖的提醒不再出现；计划文件提醒追加在保留消息之后并持久化。
                let offset = session.context.offset;
                session
                    .runtime
                    .reminders
                    .retain(|(anchor, _, _)| *anchor >= offset);
                for reminder in reminders {
                    session.append_message(reminder);
                }
                session.context_tokens = Some(tokens);
                // Node：轮内压缩的 ModelComplete 在本轮事件里，目标的 tokensUsed 包含它。
                if let Some(goal) = session.goal.as_mut() {
                    goal.account(&usage, self.clock.now());
                }
                // 修复：压缩摘要请求不是 main_turn，原先把它计入 cumulative；前后 token 也只估算
                // 会话消息。Node 的标记与 context 水位都含请求前缀，cumulative 不变（spec
                // rust-m9-usage-logs §4.2）。
                let tokens = tokens + prefix;
                let summary = session.runtime.compact_summary.take();
                let window = changed
                    .then(|| self.model_window(&self.sessions[session_id]))
                    .flatten();
                let session = self.sessions.get_mut(session_id).unwrap();
                if changed {
                    super::usage_state::compacted(session, tokens, window);
                    compact_complete = Some((body, usage));
                }
                let row = session
                    .rows
                    .iter_mut()
                    .find(|r| r["entityId"] == id)
                    .ok_or_else(|| anyhow::anyhow!("Compaction marker missing"))?;
                row["marker"]["status"] = if changed { "success" } else { "noop" }.into();
                row["marker"]["tokensAfter"] = tokens.into();
                if let Some(summary) = summary.filter(|_| changed) {
                    row["marker"]["summaryRef"] = summary.into();
                }
                deltas.push(json!({"op":"row.upserted","row":row}));
                receipt = Some(committed);
                session.revision += 1;
            }
            Event::CompactFailed { id, committed } => {
                let row = session
                    .rows
                    .iter_mut()
                    .find(|r| r["entityId"] == id)
                    .ok_or_else(|| anyhow::anyhow!("Compaction marker missing"))?;
                row["marker"]["status"] = "failed".into();
                if row["marker"]["origin"] == "auto" {
                    session.runtime.compact_failures += 1;
                }
                deltas.push(json!({"op":"row.upserted","row":row}));
                receipt = Some(committed);
            }
            _ => unreachable!(),
        }
        if let Some((summary, usage)) = compact_complete {
            self.legacy_compact_complete(id, &turn, &summary, &usage);
        }
        self.publish(id, deltas)?;
        if let Some(receipt) = receipt {
            self.persist(id, None).await?;
            let _ = receipt.send(());
        }
        Ok(())
    }
}
