use super::session::Session;
use serde_json::{Value, json};
use std::collections::HashMap;

impl Session {
    pub fn sync_subagent_row(&mut self, agent: &str) -> Option<Value> {
        let task = self.children.get(agent)?;
        let anchor = self
            .rows
            .iter()
            .find(|r| r["kind"] == "toolCall" && r["toolCallId"] == task.call_id)?;
        let turn = anchor["turnId"].as_str()?.to_owned();
        let existing = self
            .rows
            .iter()
            .position(|r| r["kind"] == "subagent" && r["childSessionId"] == task.child_id);
        self.project_subagent_row(agent, &turn, existing)
    }
    fn project_subagent_row(
        &mut self,
        agent: &str,
        turn: &str,
        existing: Option<usize>,
    ) -> Option<Value> {
        let task = self.children.get(agent)?;
        let fields = json!({"parentToolCallId":task.call_id,"childSessionId":task.child_id,
            "subagentType":task.agent_type,"summaryText":task.description,
            "status":match task.status.as_str(){"running"=>"running","completed"=>"success","failed"=>"failed",_=>"cancelled"},
            "startedAt":task.started_at});
        let ended = task.ended_at;
        let background = task.background;
        let started = task.started_at;
        // App 将 subagent 行与 Agent toolCall 精确配对后才生成详情按钮；单独 children 状态不能替代展示行。
        let mut row = if let Some(index) = existing {
            self.rows[index].clone()
        } else {
            self.row("subagent", turn, agent, started)
        };
        let object = row.as_object_mut().unwrap();
        object.extend(fields.as_object().unwrap().clone());
        object.remove("endedAt");
        if let Some(at) = ended {
            object.insert("endedAt".into(), at.into());
        }
        if background {
            object.insert("backgrounded".into(), true.into());
            object.insert("workId".into(), agent.into());
        }
        if let Some(index) = existing {
            if self.rows[index] == row {
                return None;
            }
            self.rows[index] = row.clone();
            self.saved_rows = self.saved_rows.min(index);
        } else {
            self.rows.push(row.clone());
        }
        Some(json!({"op":if existing.is_some(){"row.upserted"}else{"row.appended"},"row":row}))
    }
    pub fn recover_subagent_rows(&mut self) {
        // 旧 Rust 已持久化 child/call 身份，冷恢复可补投影；不重跑模型，也不猜测缺失的工具锚点。
        if self.children.is_empty() {
            return;
        }
        // 冷恢复一次扫描建索引，避免大历史按每个历史 child 重扫 rows 退化为平方复杂度。
        let mut anchors = HashMap::new();
        let mut existing = HashMap::new();
        for (index, row) in self.rows.iter().enumerate() {
            if row["kind"] == "toolCall" {
                if let (Some(call), Some(turn)) =
                    (row["toolCallId"].as_str(), row["turnId"].as_str())
                {
                    anchors.entry(call).or_insert(turn);
                }
            } else if row["kind"] == "subagent"
                && let Some(child) = row["childSessionId"].as_str()
            {
                existing.entry(child).or_insert(index);
            }
        }
        let updates = self
            .children
            .iter()
            .filter_map(|(id, task)| {
                anchors.get(task.call_id.as_str()).map(|turn| {
                    (
                        id.clone(),
                        (*turn).to_owned(),
                        existing.get(task.child_id.as_str()).copied(),
                    )
                })
            })
            .collect::<Vec<_>>();
        for (agent, turn, index) in updates {
            self.project_subagent_row(&agent, &turn, index);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_parent() -> Session {
        let mut s = Session::new(
            "parent".into(),
            "/local".into(),
            "fixture".into(),
            "model".into(),
            "low".into(),
            "epoch".into(),
            1,
        );
        s.rows
            .push(json!({"kind":"toolCall","rowId":"row-1","turnId":"turn","toolCallId":"launch"}));
        s.messages
            .push(json!({"role":"user","content":"unchanged"}));
        s.children.insert(
            "agent".into(),
            serde_json::from_value(json!({
                "id":"agent","childId":"child","parentRun":"run","callId":"launch",
                "agentType":"Explore","description":"inspect","prompt":"child prompt",
                "status":"completed","background":true,"notified":true,"startedAt":1,"endedAt":2,
                "output":"child result","outputFile":"output","toolUses":0,"tokens":1
            }))
            .unwrap(),
        );
        s
    }

    #[test]
    fn legacy_recovery_repairs_only_proven_anchors_and_is_idempotent() {
        let mut s = legacy_parent();
        let messages = s.messages.clone();
        s.recover("recovered".into(), 3);
        assert_eq!(s.rows.len(), 2);
        let row = s.rows[1].clone();
        assert_eq!(row["turnId"], "turn");
        assert_eq!(row["parentToolCallId"], "launch");
        assert_eq!(row["childSessionId"], "child");
        assert_eq!(row["status"], "success");
        s.recover("again".into(), 4);
        assert_eq!(s.rows.len(), 2);
        assert_eq!(s.rows[1], row);
        assert_eq!(s.messages, messages);
        let mut unanchored = legacy_parent();
        unanchored.rows.clear();
        unanchored.recover("recovered".into(), 3);
        assert!(unanchored.rows.is_empty());
    }

    #[test]
    fn background_lifecycle_survives_parent_finish_and_resume_keeps_original_row() {
        let mut s = legacy_parent();
        s.recover_subagent_rows();
        let row_id = s.rows[1]["rowId"].clone();
        let task = s.children.get_mut("agent").unwrap();
        task.status = "running".into();
        task.ended_at = None;
        assert_eq!(s.sync_subagent_row("agent").unwrap()["op"], "row.upserted");
        s.finish_rows("success", 4);
        assert_eq!(s.rows[1]["status"], "running");
        assert!(s.rows[1].get("endedAt").is_none());
        s.recover("cold".into(), 5);
        assert_eq!(s.children["agent"].status, "lost");
        assert_eq!(s.rows[1]["status"], "cancelled");
        assert_eq!(s.rows[1]["rowId"], row_id);
        assert_eq!(s.rows[1]["parentToolCallId"], "launch");
    }
}
