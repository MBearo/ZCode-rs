//! Legacy `session/send` and `session/compact` (Node `server-operations.ts`
//! `sendSession` / `compactSession`): adapters over the V4 command path. The
//! Node `activeAbortController` is `Active.legacy_lock` (spec 9.12).
use super::Engine;
use super::legacy_session::params_error;
use crate::{
    contract::RuntimeError,
    domain::{
        legacy_attachments::{self, Source},
        legacy_input_params,
        legacy_stream::RunKind,
        protocol::Command,
        session::Session,
        zod::js_trim,
    },
};
use anyhow::Result;
use serde_json::{Value, json};

/// Node `ensureNoActiveTurn` and friends: `-32010` without data.
pub(super) fn busy(message: &str) -> anyhow::Error {
    RuntimeError::Coded {
        code: -32010,
        message: message.into(),
    }
    .into()
}

/// A legacy-issued command's ACK: accepted, duplicate and no-op go on;
/// anything else fails the request with the reason (`-32603` when the
/// capability is missing, otherwise `-32010`).
pub(super) fn ack_error(ack: &Value) -> Option<anyhow::Error> {
    let status = ack["status"].as_str().unwrap_or_default();
    if matches!(status, "accepted" | "duplicate" | "noop") {
        return None;
    }
    let reason = ack["reasonCode"].as_str().unwrap_or(status);
    let code = if reason == "guard.capabilityUnsupported" {
        -32603
    } else {
        -32010
    };
    Some(
        RuntimeError::Rejected {
            code,
            message: ack["message"].as_str().unwrap_or(reason).into(),
            data: json!({"reasonCode": reason}),
        }
        .into(),
    )
}

/// What started the run in `turn`, decided once when it starts.
pub(super) fn run_kind(s: &Session, turn: &str) -> RunKind {
    let header = s
        .rows
        .iter()
        .rev()
        .find(|r| r["kind"] == "turnHeader" && r["turnId"] == turn);
    if header.is_some_and(|h| h["executionKind"] == "controlOnly") {
        return RunKind::Compact;
    }
    let continued = header
        .is_some_and(|h| h["origin"] == "goalContinuation" && h["sourceCommandId"].is_string());
    let set = s
        .history
        .inputs
        .last()
        .is_some_and(|i| i.turn == turn && i.kind == "sendGoalCommand");
    if continued || set {
        RunKind::Goal
    } else {
        RunKind::Prompt
    }
}

impl Engine {
    /// Node `requireSession`: `-32004` unless resident. Returns the id.
    pub(super) fn legacy_resident(&self, p: &Value) -> Result<String> {
        let id = p["sessionId"].as_str().unwrap_or_default().to_owned();
        if !self.sessions.contains_key(&id) {
            return Err(RuntimeError::Coded {
                code: -32004,
                message: format!("Session is not active: {id}"),
            }
            .into());
        }
        Ok(id)
    }

    /// Node `assertExpectedRevision` against the legacy `stateRevision`.
    pub(super) fn legacy_expect(&self, id: &str, p: &Value) -> Result<()> {
        let actual = self.sessions[id].runtime.state_revision;
        match p["expectedRevision"].as_u64() {
            Some(expected) if expected != actual => Err(RuntimeError::Rejected {
                code: -32009,
                message: "Session state revision mismatch".into(),
                data: json!({"actualRevision": actual, "expectedRevision": expected}),
            }
            .into()),
            _ => Ok(()),
        }
    }

    pub(super) fn legacy_locked(&self, id: &str) -> bool {
        self.active.get(id).is_some_and(|a| a.legacy_lock)
    }

    /// A command issued on the Host's behalf; `inputId` is its command id.
    pub(super) fn legacy_envelope(
        &self,
        id: &str,
        (client, kind): (&str, &str),
        input: Option<&str>,
        payload: Value,
    ) -> Command {
        Command {
            command_id: input.map_or_else(|| self.clock.id(), str::to_owned),
            client_id: client.into(),
            session_id: Some(id.into()),
            base_revision: None,
            base_log_epoch: None,
            kind: kind.into(),
            payload,
            issued_at: self.clock.now() as f64,
        }
    }

    pub(super) async fn legacy_send(&mut self, raw: &Value) -> Result<Value> {
        let p = legacy_input_params::send(raw).map_err(params_error)?;
        let id = self.legacy_resident(&p)?;
        if self.sessions[&id].task_type == "subagent_child" {
            return Err(RuntimeError::Rejected {
                code: -32010,
                message: "Subagent sessions are read-only".into(),
                data: json!({"reasonCode": "guard.subagentReadOnly"}),
            }
            .into());
        }
        self.legacy_expect(&id, &p)?;
        if self.legacy_locked(&id) {
            return Err(busy("A prompt is already running for this session"));
        }
        self.touch_session(&id);
        let (refs, staged) = self.legacy_stage(&id, &p["attachments"]).await?;
        let mut payload = json!({"text": p["content"], "heldQueueDisposition": "keepQueueAndSend"});
        for key in [
            "modelSelection",
            "modelExecution",
            "browserAmbientContext",
            "automationId",
            "offPeakTaskId",
            "offPeakRunType",
        ] {
            if let Some(value) = p.get(key) {
                payload[key] = value.clone();
            }
        }
        if let Some(tools) = p.get("toolDenylist") {
            payload["toolDisallowlist"] = tools.clone();
        }
        if self.sessions[&id].running() {
            // Node Core admission：未持锁的运行中，有附件排队，否则引导进当前轮。
            payload["requestedDelivery"] = if refs.is_empty() { "guide" } else { "queue" }.into();
        }
        if !refs.is_empty() {
            payload["attachments"] = refs.into();
        }
        let input = p["inputId"].as_str();
        let command =
            self.legacy_envelope(&id, ("legacy-session-send", "sendText"), input, payload);
        let key = command.key();
        let ack = match self.command(command).await {
            Ok(ack) => ack,
            Err(error) => {
                // 命令未提交时撤销本次登记；已提交（ACK 已缓存）时行里引用着这些附件，保留。
                if !self.acks.contains_key(&key) {
                    self.unstage(&id, &staged);
                }
                return Err(error);
            }
        };
        let started = ack["status"] == "accepted" && ack["result"]["delivery"] == "startNow";
        let end = if started {
            if let Some(active) = self.active.get_mut(&id) {
                active.legacy_lock = true;
            }
            None
        } else if ack["status"] == "failed" && ack["reasonCode"] == "activePrompt" {
            // 与 Node 一致：忙时带 modelExecution 的输入先 ACK，后台 admission 失败后静默丢弃。
            self.unstage(&id, &staged);
            Some("prompt_failed")
        } else if let Some(error) = ack_error(&ack) {
            self.unstage(&id, &staged);
            return Err(error);
        } else {
            if ack["status"] == "duplicate" {
                self.unstage(&id, &staged);
            }
            // 排队、引导或重复提交：Node 的后台在此时返回，随即报告 prompt_completed。
            Some("prompt_completed")
        };
        self.legacy_state_patch(&id, "prompt_started", Some(json!({"status": "running"})))?;
        let revision = self.sessions[&id].runtime.state_revision;
        if let Some(reason) = end {
            self.legacy_state_updated(&id, reason)?;
        }
        Ok(json!({"sessionId": id, "accepted": true, "stateRevision": revision}))
    }

    /// Maps the Host attachments (spec 9.12); inline bytes become session
    /// artifacts. Returns the V4 refs and the references it registered.
    async fn legacy_stage(&mut self, id: &str, items: &Value) -> Result<(Vec<Value>, Vec<String>)> {
        let mut refs = vec![];
        let mut staged = vec![];
        for item in items.as_array().into_iter().flatten() {
            let Some(mapped) = legacy_attachments::map(item) else {
                continue;
            };
            let (reference, bytes) = match mapped.source {
                Source::Path(path) => (path, 0),
                Source::Bytes(bytes) => {
                    let len = bytes.len() as u64;
                    let call = format!("attachment-{}", refs.len() + 1);
                    let stored = self
                        .store
                        .put_attachment(id, &call, &[bytes], &mapped.mime)
                        .await;
                    let (reference, asset) = match stored {
                        Ok(stored) => stored,
                        Err(error) => {
                            self.unstage(id, &staged);
                            return Err(error);
                        }
                    };
                    self.register_attachment(id, reference.clone(), asset)?;
                    staged.push(reference.clone());
                    (reference, len)
                }
            };
            refs.push(json!({"ref": reference, "fileName": mapped.file_name,
                "mime": mapped.mime, "bytes": bytes}));
        }
        Ok((refs, staged))
    }

    fn unstage(&mut self, id: &str, staged: &[String]) {
        if let Some(s) = self.sessions.get_mut(id) {
            for reference in staged {
                s.attachments.remove(reference);
            }
        }
    }

    pub(super) async fn legacy_compact(&mut self, raw: &Value) -> Result<Value> {
        let p = legacy_input_params::compact(raw).map_err(params_error)?;
        let id = self.legacy_resident(&p)?;
        self.legacy_expect(&id, &p)?;
        self.touch_session(&id);
        let mut compact = json!({"state": "accepted"});
        if let Some(input) = p.get("inputId") {
            compact["inputId"] = input.clone();
        }
        if self
            .active
            .get(&id)
            .is_some_and(|a| a.kind == RunKind::Compact)
        {
            compact["state"] = "already_running".into();
            let snapshot = self.legacy_snapshot_with(&id, false)?;
            return Ok(json!({"response": "", "snapshot": snapshot, "compact": compact}));
        }
        if self.legacy_locked(&id) {
            return Err(busy("Cannot compact while a prompt is running"));
        }
        let text = js_trim(p["instructions"].as_str().unwrap_or_default());
        let command = self.legacy_envelope(
            &id,
            ("legacy-session", "compact"),
            p["inputId"].as_str(),
            json!({"text": text}),
        );
        // 与 Engine::command 相同的去重；legacy 压缩在空闲时越过暂停的队列立即开始。
        let ack = match self.cached_ack(&command.key()).await? {
            Some(mut ack) => {
                if ack["status"] == "accepted" {
                    ack["status"] = "duplicate".into();
                }
                ack
            }
            None => self.compact_command(&command, true).await?,
        };
        if let Some(error) = ack_error(&ack) {
            return Err(error);
        }
        self.legacy_state_patch(&id, "compact_started", Some(json!({"status": "running"})))?;
        let snapshot = self.legacy_snapshot_with(&id, false)?;
        Ok(json!({"response": "", "snapshot": snapshot, "compact": compact}))
    }
}
