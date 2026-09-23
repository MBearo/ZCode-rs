use super::session::Session;
use serde_json::json;
impl Session {
    pub fn close_unfinished_tools(&mut self) {
        let mut unresolved = std::collections::BTreeMap::new();
        for message in &self.messages {
            if let Some(calls) = message["tool_calls"].as_array() {
                for call in calls {
                    unresolved.insert(call["id"].as_str().unwrap_or("").to_owned(), ());
                }
            }
            if let Some(id) = message["tool_call_id"].as_str() {
                unresolved.remove(id);
            }
        }
        for (id, _) in unresolved {
            let answered = self.rows.iter().find(|r| {
                r["toolCallId"] == id
                    && matches!(
                        r["toolName"].as_str(),
                        Some("AskUserQuestion" | "TodoRead" | "TodoWrite")
                    )
                    && matches!(r["status"].as_str(), Some("success" | "error"))
                    && r["output"]["text"].is_string()
            });
            let content=answered.map(|r|r["output"]["text"].clone()).unwrap_or_else(||json!("Interrupted; execution outcome is unknown. Do not assume this action was not performed."));
            let failed = answered.is_some_and(|r| r["status"] == "error");
            self.append_message(json!({"role":"tool","tool_call_id":id,"content":content,"_zcode_tool_failed":failed}));
        }
    }
    pub fn finish_rows(&mut self, outcome: &str, now: u64) {
        for row in &mut self.rows {
            if row["kind"] == "timelineMarker"
                && row["marker"]["type"] == "goalVerify"
                && row["marker"]["outcome"] == "running"
            {
                row["marker"]["outcome"] = "failed".into();
                row["marker"]["detail"] = "Goal verification interrupted".into();
                if let Some(goal) = &mut self.goal {
                    goal.verifications.push(json!({"iteration":row["marker"]["iteration"],"outcome":"failed","at":now,"anchorRowId":row["rowId"],"reason":"Goal verification interrupted"}));
                }
            }
            if row["kind"] == "timelineMarker"
                && row["marker"]["type"] == "compact"
                && row["marker"]["status"] == "running"
            {
                row["marker"]["status"] = if outcome == "interrupted" {
                    "cancelled"
                } else {
                    "failed"
                }
                .into();
            }
            if row["state"] == "streaming" {
                row["state"] = if outcome == "success" {
                    "complete"
                } else {
                    "interrupted"
                }
                .into();
            }
            if row["kind"] == "turnHeader" && row["state"] == "running" {
                row["state"] = match outcome {
                    "success" => "completedSuccess",
                    "interrupted" => "completedInterrupted",
                    _ => "failed",
                }
                .into();
                row["endedAt"] = now.into();
            }
            // 子代理由独立 child 生命周期收口；父回复结束不能把仍在运行的后台 child 标成取消。
            if row["kind"] != "subagent"
                && matches!(row["status"].as_str(), Some("running" | "pendingApproval"))
            {
                row["status"] = "cancelled".into();
                row.as_object_mut().unwrap().remove("approvalInteractionId");
            }
        }
    }
}
