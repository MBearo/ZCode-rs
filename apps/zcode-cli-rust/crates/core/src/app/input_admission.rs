use super::Engine;
use crate::domain::protocol::Command;
use anyhow::{Context, Result};
use serde_json::{Value, json};

impl Engine {
    pub(super) fn admit_input(
        &mut self,
        id: &str,
        c: &Command,
        shared: Option<String>,
    ) -> Result<(String, String)> {
        if c.kind == "sendText"
            && let Some(text) = c.payload["text"].as_str()
        {
            let text = text.trim();
            if text == "/compact" || text.starts_with("/compact ") {
                let mut compact = c.clone();
                compact.payload = json!({"text":text.strip_prefix("/compact").unwrap().trim()});
                return self.admit_compact(id, &compact);
            }
        }
        let selected = self.select(&c.payload, Some(self.session_selection(id)?))?;
        let (mut content, reminders) = self.input_content(id, &c.payload)?;
        let request_content = super::submission::ambient_content(
            &content,
            c.payload["text"].as_str().unwrap_or(""),
            &c.payload,
        );
        if c.payload["_userSteer"] == true
            && let Some(text) = content.as_str()
        {
            content = crate::domain::prompt::user_steer(text).into();
        }
        // 与 Node 一致：执行级选型只用于本轮，不写入会话选择。
        let execution = super::submission::execution(&c.payload, selected.clone());
        if execution.is_none() {
            self.apply_selection(id, selected)?;
        }
        // Node resolveSubmittedExecutionState：本次输入的模式在开轮时生效，auto 按 build 提交。
        let submitted = self.submitted_execution_state(id, &c.payload)?;
        self.apply_execution_state(
            id,
            Some(submitted.mode.as_str()),
            Some(submitted.plan_enabled),
            None,
        )?;
        let s = self.sessions.get_mut(id).context("Session unavailable")?;
        let boundary = (
            s.rows.len(),
            s.messages.len(),
            crate::domain::history::State::capture(s),
        );
        let now = self.clock.now();
        let turn = self.clock.id();
        let input = self.clock.id();
        let text = c.payload["text"].as_str().context("Input text missing")?;
        let goal_set = c.kind == "sendGoalCommand";
        // Node：本进程没有运行时遗留的目标运行，按中断结算（recoverInterruptedSessionTargetRun）。
        if let Some(goal) = s.goal.as_mut() {
            goal.recover(now);
        }
        // /goal 是 control-only 轮次（userInput 行在它自己的轮次），续跑轮用本次运行的 turn。
        let control = if goal_set {
            let paused = s.goal.as_ref().is_some_and(|g| g.target_status == "paused");
            s.node.goal_resumed = paused;
            let target = crate::domain::node_ids::target_id(now, &self.clock.id());
            let mut goal = crate::domain::goal::Goal::new(target, text.trim().into(), now);
            goal.loop_input = Some(c.command_id.clone());
            s.goal = Some(goal);
            content = super::goal_commands::display_text(c).into();
            self.clock.id()
        } else {
            turn.clone()
        };
        // Node：目标 active 时每一轮都记运行时间与 token（startSessionTargetRun）。
        if let Some(goal) = s.goal.as_mut() {
            goal.start_run(&c.command_id, now);
        }
        super::shared_context::attach(
            s,
            &c.payload,
            shared,
            Some(&format!("queue_{}", c.command_id)),
            &input,
        )?;
        let retained_messages = s.messages.len();
        s.run_id = Some(self.clock.id());
        s.phase = crate::domain::execution::Phase::Running;
        s.last_error = None;
        s.updated_at = now;
        s.revision += 1;
        if s.title.is_empty() {
            s.title = if text.trim().is_empty() {
                c.payload["attachments"][0]["fileName"]
                    .as_str()
                    .unwrap_or("Attachment")
                    .chars()
                    .take(80)
                    .collect()
            } else {
                text.chars().take(80).collect()
            };
            s.title_source = "generated".into();
        }
        let mut header = s.row("turnHeader", &control, &control, now);
        header["origin"] = if c.payload["_historyRerun"] == true {
            "editRerun"
        } else {
            "userInput"
        }
        .into();
        header["state"] = "running".into();
        header["startedAt"] = now.into();
        header["sourceCommandId"] = c.command_id.clone().into();
        if goal_set {
            header["executionKind"] = "controlOnly".into();
            header["state"] = "completedSuccess".into();
            header["endedAt"] = now.into();
        }
        s.rows.push(header);
        let mut row = s.row("userInput", &control, &input, now);
        row["text"] = text.into();
        if goal_set {
            row["text"] = super::goal_commands::display_text(c).into();
        }
        row["origin"] = "realUser".into();
        row["sourceCommandId"] = c.command_id.clone().into();
        row["clientId"] = c.client_id.clone().into();
        if let Some(refs) = c.payload.get("attachments") {
            row["attachments"] = refs.clone();
        }
        s.rows.push(row);
        if !s.background.is_empty() {
            let statuses = s
                .background
                .values()
                .map(|t| json!({"task_id":t.id,"status":t.status,"outputFile":t.output_file}))
                .collect::<Vec<_>>();
            s.append_message(json!({"role":"user","content":format!("<task-notification>{}</task-notification>", serde_json::to_string(&statuses)?)}));
        }
        let mut message = json!({"role":"user","content":content});
        if let Some(request) = request_content {
            // 浏览器环境上下文只进入模型请求；落盘与 rows 保留用户原文（重启后消失，同 Node）。
            message["_zcode_request_content"] = request;
        }
        s.append_message(message);
        // Node：文本附件以 prompt_attachment 提醒跟在 user 消息之后。
        for body in reminders {
            let source = "prompt_attachment";
            s.append_message(json!({"role":"user","_zcode_source":source,
                "content":crate::domain::node_history::reminders::wrap(source, &body)}));
        }
        let continued = if goal_set {
            let goal = s.goal.clone().unwrap();
            let source = Some(c.command_id.as_str());
            let message = super::goal_commands::continuation(s, &goal, None, (&turn, source), now);
            message["content"].as_str().map(str::to_owned)
        } else {
            None
        };
        let mut payload = c.payload.clone();
        payload.as_object_mut().unwrap().remove("context_refs");
        // 凭据与单次执行选项不属于可持久化的 intent。
        super::submission::strip_transient(&mut payload);
        s.history
            .inputs
            .push(crate::domain::history::InputBoundary {
                entity: input.clone(),
                turn: control.clone(),
                row: boundary.0,
                user_row: boundary.0 + 1,
                message: retained_messages,
                state: boundary.2,
                kind: c.kind.clone(),
                payload,
                node_message: None,
            });
        // 每个会话同一时刻至多一个待启动的输入；失败路径遗留的旧条目在此清除。
        self.submissions.retain(|(session, _), _| session != id);
        let mut submission =
            super::submission::Submission::new(&c.payload, &c.command_id, execution);
        // Node runUserPromptSubmitHooks 的 prompt 是提交给模型的输入：/goal 为目标续跑提示。
        let prompt = continued.unwrap_or_else(|| text.to_owned());
        let attachments = super::turn_hooks::attachments_summary(&c.payload["attachments"]);
        submission.prompt = Some((prompt, attachments));
        self.submissions
            .insert((id.to_owned(), turn.clone()), submission);
        Ok((turn, input))
    }
    pub(super) fn new_turn_rows(&self, id: &str) -> Vec<Value> {
        let rows = &self.sessions[id].rows;
        let mut start = self.sessions[id].current_rows_start();
        // /goal 的 control-only 轮次（header + userInput）与续跑轮同时开始，一并发布。
        if start >= 2
            && rows[start]["origin"] == "goalContinuation"
            && rows[start - 2]["executionKind"] == "controlOnly"
            && rows[start - 2]["createdAt"] == rows[start]["createdAt"]
        {
            start -= 2;
        }
        rows[start..]
            .iter()
            .map(|row| json!({"op":"row.appended","row":row}))
            .collect()
    }
}
