use super::Engine;
use crate::{
    contract::{ChildHandle, Event, ModelIdentity, StorageCommitFailure},
    domain::{
        protocol::Command,
        session::Session,
        subagent::{Profile, Task},
    },
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

impl Engine {
    pub(super) async fn subagent_event(&mut self, parent: &str, event: Event) -> Result<()> {
        let Event::Subagent {
            name,
            args,
            call_id,
            profile,
            selection,
            reply,
        } = event
        else {
            unreachable!()
        };
        let result = if let Some(profile) = profile {
            self.launch_child(parent, &call_id, &args, *profile, selection)
                .await
        } else {
            self.child_action(parent, &name, &args).await
        };
        match result {
            Err(error) if error.is::<StorageCommitFailure>() => Err(error),
            result => {
                let _ = reply.send(result.map_err(|e| e.to_string()));
                Ok(())
            }
        }
    }
    async fn launch_child(
        &mut self,
        parent: &str,
        call: &str,
        args: &Value,
        profile: Profile,
        selection: Option<ModelIdentity>,
    ) -> Result<ChildHandle> {
        ensure!(
            self.sessions[parent]
                .children
                .values()
                .filter(|t| t.running())
                .count()
                < 4,
            "Subagent concurrency limit reached"
        );
        ensure!(
            self.sessions
                .values()
                .filter(|s| s.agent_profile.is_some() && s.running())
                .count()
                < 32,
            "Workspace subagent concurrency limit reached"
        );
        let mut depth = 0;
        let mut ancestor = Some(parent);
        while let Some(id) = ancestor {
            depth += 1;
            ensure!(depth <= 4, "Subagent depth limit reached");
            ancestor = self.sessions.get(id).and_then(|s| s.parent_id.as_deref());
        }
        let selection = self.select(
            &profile
                .model_selection
                .as_ref()
                .map(|s| json!({"modelSelection":s}))
                .unwrap_or(json!({})),
            selection.or(Some(self.session_selection(parent)?)),
        )?;
        let now = self.clock.now();
        let agent = format!("agent_{}", self.clock.id());
        let child = format!("subagent_{agent}");
        let output_file = self.tools.agent_output(&child, "").await?;
        self.tools.inherit_session(parent, &child).await?;
        let mut session = Session::new(
            child.clone(),
            self.workspace.clone(),
            selection.provider_id,
            selection.model_id,
            selection.reasoning_level,
            self.clock.id(),
            now,
        );
        session.parent_id = Some(parent.into());
        session.task_type = "subagent_child".into();
        session.listed = false;
        session.title = args["description"].as_str().unwrap().into();
        session.title_source = "custom".into();
        session.workspace_path = Some(self.workspace_path.clone());
        session.skills = self.sessions[parent].skills.clone();
        session.prompt_snapshot = self.sessions[parent].prompt_snapshot.clone();
        session.agent_profile = Some(profile.clone());
        self.sessions.insert(child.clone(), session);
        let c = child_command(&child, &self.clock.id(), args["prompt"].as_str().unwrap());
        let (turn, _) = self.admit_input(&child, &c, None)?;
        self.persist(&child, None).await?;
        let task = Task {
            id: agent.clone(),
            child_id: child.clone(),
            parent_run: self.active[parent].run_id.clone(),
            call_id: call.into(),
            agent_type: profile.name,
            description: args["description"].as_str().unwrap().into(),
            prompt: args["prompt"].as_str().unwrap().into(),
            status: "running".into(),
            background: args["run_in_background"] == true || profile.background,
            notified: false,
            started_at: now,
            ended_at: None,
            output: String::new(),
            output_file,
            tool_uses: 0,
            tokens: 0,
        };
        self.sessions
            .get_mut(parent)
            .unwrap()
            .children
            .insert(agent.clone(), task.clone());
        self.sessions.get_mut(parent).unwrap().revision += 1;
        let deltas = self
            .sessions
            .get_mut(parent)
            .unwrap()
            .sync_subagent_row(&agent)
            .into_iter()
            .collect();
        self.publish(parent, deltas)?;
        self.persist(parent, None).await?;
        let handle = self.child_handle(task, None, None);
        self.start_run(&child, turn)?;
        Ok(handle)
    }
    pub(super) fn child_handle(
        &mut self,
        task: Task,
        message_id: Option<String>,
        delivery: Option<String>,
    ) -> ChildHandle {
        let updates = self
            .child_updates
            .entry(task.child_id.clone())
            .or_insert_with(|| tokio::sync::watch::channel(task.clone()).0);
        updates.send_replace(task.clone());
        ChildHandle {
            task,
            updates: updates.subscribe(),
            message_id,
            delivery,
        }
    }
    async fn child_action(
        &mut self,
        parent: &str,
        name: &str,
        args: &Value,
    ) -> Result<ChildHandle> {
        let key = if name == "SendMessage" {
            "to"
        } else {
            "task_id"
        };
        let agent = args[key].as_str().context("Agent ID required")?;
        let task = self.sessions[parent]
            .children
            .get(agent)
            .context("Task unavailable in this session")?
            .clone();
        let mut message_id = None;
        let mut delivery = None;
        if name == "TaskStop" && task.running() {
            self.cancel_children(&task.child_id).await?;
            if let Some(active) = self.active.get(&task.child_id) {
                active.cancel.cancel();
            }
            self.tools.cancel_session(&task.child_id, None).await?;
        } else if name == "SendMessage" {
            self.ensure_session(&task.child_id).await?;
            let id = self.clock.id();
            let content = format!(
                "{}\n\n{}",
                args["summary"].as_str().unwrap(),
                args["message"].as_str().unwrap()
            );
            if self.sessions[&task.child_id].running() {
                let child = self.sessions.get_mut(&task.child_id).unwrap();
                ensure!(child.mailbox.len() < 32, "Child mailbox is full");
                child.mailbox.push(json!({"id":id,"text":content}));
                child.revision += 1;
                self.persist(&task.child_id, None).await?;
                delivery = Some("queued".into());
            } else {
                ensure!(
                    self.sessions[parent]
                        .children
                        .values()
                        .filter(|t| t.running())
                        .count()
                        < 4,
                    "Subagent concurrency limit reached"
                );
                self.tools.inherit_session(parent, &task.child_id).await?;
                let c = child_command(&task.child_id, &id, &content);
                let (turn, _) = self.admit_input(&task.child_id, &c, None)?;
                let child = self.sessions.get_mut(&task.child_id).unwrap();
                if let Some(row) = child.rows.last_mut() {
                    row["origin"] = "mailbox".into();
                }
                self.publish(&task.child_id, self.new_turn_rows(&task.child_id))?;
                self.persist(&task.child_id, None).await?;
                let now = self.clock.now();
                let task = self
                    .sessions
                    .get_mut(parent)
                    .unwrap()
                    .children
                    .get_mut(agent)
                    .unwrap();
                task.status = "running".into();
                task.background = true;
                task.notified = false;
                task.started_at = now;
                task.ended_at = None;
                task.output.clear();
                task.parent_run = self.active[parent].run_id.clone();
                self.sessions.get_mut(parent).unwrap().revision += 1;
                let deltas = self
                    .sessions
                    .get_mut(parent)
                    .unwrap()
                    .sync_subagent_row(agent)
                    .into_iter()
                    .collect();
                self.publish(parent, deltas)?;
                self.persist(parent, None).await?;
                let task = self.sessions[parent].children[agent].clone();
                self.child_handle(task, None, None);
                self.start_run(&c.session_id.unwrap(), turn)?;
                delivery = Some("resumed_background".into());
            }
            message_id = Some(id);
        }
        Ok(self.child_handle(
            self.sessions[parent].children[agent].clone(),
            message_id,
            delivery,
        ))
    }
    pub(super) async fn cancel_children(&mut self, parent: &str) -> Result<()> {
        let mut pending = vec![parent.to_owned()];
        let mut children = vec![];
        while let Some(id) = pending.pop() {
            if let Some(s) = self.sessions.get(&id) {
                for task in s.children.values().filter(|t| t.running()) {
                    pending.push(task.child_id.clone());
                    children.push(task.child_id.clone());
                }
            }
        }
        for child in children {
            if let Some(s) = self.sessions.get_mut(&child) {
                s.auto_drain = false;
                s.mailbox.clear();
            }
            if let Some(active) = self.active.get(&child) {
                active.cancel.cancel();
            }
            self.cancel_auth(&child);
            self.tools.cancel_session(&child, None).await?;
        }
        Ok(())
    }
    pub(super) async fn subagents_query(&mut self, p: &Value) -> Result<Value> {
        let id = p["sessionId"].as_str().context("Session required")?;
        self.ensure_session(id).await?;
        let s = &self.sessions[id];
        let offset = p["endedCursor"]
            .as_str()
            .map(str::parse::<usize>)
            .transpose()?
            .unwrap_or(0);
        let limit = p["endedLimit"].as_u64().unwrap_or(20);
        ensure!((1..=100).contains(&limit), "Invalid ended limit");
        let mut ended = s
            .children
            .values()
            .filter(|t| !t.running())
            .collect::<Vec<_>>();
        ended.sort_by_key(|t| std::cmp::Reverse(t.ended_at));
        ensure!(offset <= ended.len(), "Invalid ended cursor");
        let items = ended
            .iter()
            .skip(offset)
            .take(limit as usize)
            .map(|t| t.summary())
            .collect::<Vec<_>>();
        let mut end = json!({"total":ended.len(),"items":items});
        if offset + items.len() < ended.len() {
            end["nextCursor"] = (offset + items.len()).to_string().into();
        }
        Ok(
            json!({"revision":s.revision,"childSessionIds":s.children.values().map(|t|&t.child_id).collect::<Vec<_>>(),"running":s.children.values().filter(|t|t.running()).map(|t|t.summary()).collect::<Vec<_>>(),"ended":end}),
        )
    }
    pub(super) async fn drain_mailbox(
        &mut self,
        id: &str,
        turn: &str,
    ) -> Result<Option<Vec<Value>>> {
        let s = self.sessions.get_mut(id).unwrap();
        if s.mailbox.is_empty() {
            return Ok(None);
        }
        let mut deltas = vec![];
        let mut messages = vec![];
        for item in std::mem::take(&mut s.mailbox) {
            let mut row = s.row(
                "userInput",
                turn,
                item["id"].as_str().unwrap(),
                self.clock.now(),
            );
            row["text"] = item["text"].clone();
            row["origin"] = "mailbox".into();
            s.rows.push(row.clone());
            deltas.push(json!({"op":"row.appended","row":row}));
            let message = json!({"role":"user","content":item["text"]});
            s.append_message(message.clone());
            messages.push(message);
        }
        s.revision += 1;
        self.publish(id, deltas)?;
        self.persist(id, None).await?;
        Ok(Some(messages))
    }
}
pub(super) fn child_command(child: &str, id: &str, text: &str) -> Command {
    Command {
        command_id: id.into(),
        client_id: "subagent-coordinator".into(),
        session_id: Some(child.into()),
        kind: "sendText".into(),
        payload: json!({"text":text}),
        issued_at: 0.0,
        base_log_epoch: None,
        base_revision: None,
    }
}
