use super::session::Session;
use serde_json::{Value, json};

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
            self.row("subagent", &turn, agent, started)
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
        let agents = self.children.keys().cloned().collect::<Vec<_>>();
        for agent in agents {
            self.sync_subagent_row(&agent);
        }
    }
}
