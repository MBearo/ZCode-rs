//! Drives the runtime actor for one `-p` prompt over the transport contract
//! (spec rust-m4-headless §3–§5). The actor owns every session fact; this side
//! only issues requests and prints what the legacy event stream reports.
use crate::args::{Format, Resume};
use crate::output::{Projection, Summary, Usage, error_line, failure};
use serde_json::{Value, json};
use std::collections::VecDeque;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, watch};
use zcode_cli_core_api::{ClientMsg, Method, RuntimeError, ServerMsg};

const CLIENT: &str = "zcode-cli-headless";

/// One prompt run.
pub struct Request {
    pub prompt: String,
    pub format: Format,
    pub mode: String,
    /// V4 `AttachmentRef` values.
    pub attachments: Vec<Value>,
    pub resume: Resume,
    pub disallowed_tools: Vec<String>,
    /// Legacy workspace ref (`workspacePath`, `workspaceKey`, `workspaceIdentity?`).
    pub workspace: Value,
    /// Shown in `No resumable session found for <directory>`.
    pub directory: String,
    pub verbose: bool,
}

/// Where output goes; stdout carries only results.
pub struct Io<O, E> {
    pub stdout: O,
    pub stderr: E,
}

/// The exit code a signal asks for (129 / 130 / 143); `None` until one arrives.
pub type Interrupt = watch::Receiver<Option<i32>>;

struct Driver<'a, O, E> {
    to: &'a mpsc::Sender<ClientMsg>,
    from: &'a mut mpsc::Receiver<Vec<ServerMsg>>,
    pending: VecDeque<ServerMsg>,
    token: u64,
    io: &'a mut Io<O, E>,
    format: Format,
    session: String,
    /// The prompt was submitted; the next `turn.started` is its turn.
    submitted: bool,
    turn: Option<String>,
    trace: Option<String>,
    terminal: Option<Value>,
    events: u64,
}

enum Failure {
    /// A request failed: `Error: <message>`.
    Request(String),
    /// Output could not be written or the runtime went away.
    Io(String),
}

impl From<RuntimeError> for Failure {
    fn from(error: RuntimeError) -> Self {
        Self::Request(error.to_string())
    }
}

impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_millis() as f64)
}

fn command(session: &str, kind: &str, payload: Value) -> Value {
    json!({"commandId": uuid::Uuid::new_v4().to_string(), "clientId": CLIENT,
        "sessionId": session, "type": kind, "payload": payload, "issuedAt": now()})
}

impl<O: AsyncWrite + Unpin, E: AsyncWrite + Unpin> Driver<'_, O, E> {
    async fn next(&mut self) -> Result<ServerMsg, Failure> {
        loop {
            if let Some(message) = self.pending.pop_front() {
                return Ok(message);
            }
            match self.from.recv().await {
                Some(batch) => self.pending.extend(batch),
                None => return Err(Failure::Io("Runtime stopped unexpectedly".into())),
            }
        }
    }

    /// Sends a request and waits for its reply, handling everything else that
    /// arrives meanwhile.
    async fn call(&mut self, method: Method, params: Value) -> Result<Value, Failure> {
        self.token += 1;
        let token = self.token;
        let request = ClientMsg::Request {
            token,
            method,
            params,
        };
        if self.to.send(request).await.is_err() {
            return Err(Failure::Io("Runtime stopped unexpectedly".into()));
        }
        loop {
            match self.next().await? {
                ServerMsg::Reply { token: t, result } if t == token => return Ok(result?),
                message => self.observe(message).await?,
            }
        }
    }

    async fn observe(&mut self, message: ServerMsg) -> Result<(), Failure> {
        match message {
            ServerMsg::HostNotification {
                method: "session/event",
                params,
            } if params["sessionId"] == self.session.as_str() => self.event(params).await?,
            // Host 反向请求在无头模式下没有应答方：与 Host 返回错误时一样以 null 回复。
            ServerMsg::HostRequest { id, .. } => {
                let reply = ClientMsg::HostReply {
                    id,
                    result: Value::Null,
                };
                let _ = self.to.send(reply).await;
            }
            _ => {}
        }
        Ok(())
    }

    async fn event(&mut self, mut event: Value) -> Result<(), Failure> {
        let kind = event["type"].as_str().unwrap_or_default().to_owned();
        if self.submitted && kind == "turn.started" && self.turn.is_none() {
            self.turn = event["turnId"].as_str().map(str::to_owned);
        }
        if let Some(trace) = event["traceId"].as_str() {
            self.trace = Some(trace.into());
        }
        if kind != "session.titleUpdated" && !kind.starts_with("permission.") {
            self.events += 1;
        }
        let ours = self.turn.is_some() && event["turnId"].as_str() == self.turn.as_deref();
        if self.format == Format::StreamJson {
            if let Some(object) = event.as_object_mut() {
                object.remove("deliveryKind");
            }
            let line = format!("{event}\n");
            self.io.stdout.write_all(line.as_bytes()).await?;
            self.io.stdout.flush().await?;
        }
        if ours && matches!(kind.as_str(), "turn.completed" | "turn.failed") {
            self.terminal = Some(event);
        }
        Ok(())
    }

    async fn open(&mut self, request: &Request) -> Result<(), Failure> {
        let resumed = match &request.resume {
            Resume::New => None,
            Resume::Id(id) => Some(id.clone()),
            Resume::Latest => {
                let listed = self
                    .call(Method::SessionList, json!({"workspace": request.workspace}))
                    .await?;
                let latest = listed["sessions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|s| s.get("parentSessionId").is_none())
                    .max_by_key(|s| s["updatedAt"].as_u64().unwrap_or(0))
                    .and_then(|s| s["sessionId"].as_str());
                let Some(latest) = latest else {
                    let message = format!("No resumable session found for {}", request.directory);
                    return Err(Failure::Request(message));
                };
                Some(latest.to_owned())
            }
        };
        self.session = match resumed {
            None => {
                let params = json!({"workspace": request.workspace, "mode": request.mode,
                    "persistence": "immediate"});
                let created = self.call(Method::SessionCreate, params).await?;
                created["session"]["sessionId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            }
            Some(id) => {
                let params = json!({"sessionId": id, "workspace": request.workspace});
                self.call(Method::SessionResume, params).await?;
                // Node 的 --mode 同样覆盖恢复会话中保存的模式。
                let params = json!({"sessionId": id, "mode": request.mode});
                self.call(Method::SessionSetMode, params).await?;
                id
            }
        };
        let params = json!({"sessionId": self.session, "deliveryKind": "desktop-continuous"});
        self.call(Method::SessionSubscribe, params).await?;
        Ok(())
    }

    async fn submit(&mut self, request: &Request) -> Result<(), Failure> {
        let mut payload = json!({"text": request.prompt});
        if !request.attachments.is_empty() {
            payload["attachments"] = request.attachments.clone().into();
        }
        // "Bash(git *)" 移除整个 Bash（Node：命令模式不参与匹配）。
        let mut tools: Vec<&str> = vec![];
        for rule in &request.disallowed_tools {
            let name = rule.split('(').next().unwrap_or(rule).trim();
            if !name.is_empty() && !tools.contains(&name) {
                tools.push(name);
            }
        }
        if !tools.is_empty() {
            payload["toolDisallowlist"] = tools.into();
        }
        self.submitted = true;
        let ack = self
            .call(Method::Command, command(&self.session, "sendText", payload))
            .await?;
        if ack["status"] != "accepted" {
            let reason = ack["message"]
                .as_str()
                .or(ack["reasonCode"].as_str())
                .unwrap_or("Prompt was not accepted");
            return Err(Failure::Request(reason.into()));
        }
        Ok(())
    }

    /// Waits for the turn's terminal event; the first signal stops the turn.
    async fn finish(&mut self, interrupt: &mut Interrupt) -> Result<Option<i32>, Failure> {
        let mut signalled = *interrupt.borrow();
        let mut stopped = false;
        while self.terminal.is_none() {
            if signalled.is_some() && !stopped {
                stopped = true;
                let stop = command(&self.session, "stop", json!({}));
                self.call(Method::Command, stop).await?;
                continue;
            }
            tokio::select! {
                message = self.next() => {
                    let message = message?;
                    self.observe(message).await?;
                }
                changed = interrupt.changed(), if signalled.is_none() => {
                    if changed.is_ok() {
                        signalled = *interrupt.borrow();
                    }
                }
            }
        }
        Ok(signalled)
    }

    async fn report(&mut self, request: &Request) -> Result<i32, Failure> {
        let terminal = self.terminal.take().unwrap_or_default();
        let payload = &terminal["payload"];
        let trace = self.trace.clone();
        if terminal["type"] == "turn.failed" {
            let (message, cause) = failure(&payload["error"]);
            self.write_error(
                &message,
                trace.as_deref(),
                cause.filter(|_| request.verbose),
            )
            .await?;
            return Ok(1);
        }
        if payload["resultType"] == "cancelled" {
            self.write_error("Turn was cancelled.", trace.as_deref(), None)
                .await?;
            return Ok(1);
        }
        let response = payload["response"].as_str().unwrap_or_default().to_owned();
        if request.format == Format::Text {
            let text = format!("{response}\n");
            self.io.stdout.write_all(text.as_bytes()).await?;
            self.io.stdout.flush().await?;
            return Ok(0);
        }
        let params = json!({"sessionId": self.session, "deliveryKind": "desktop-continuous",
            "includeSnapshot": true});
        let subscribed = self.call(Method::SessionSubscribe, params).await?;
        let stream = request.format == Format::StreamJson;
        let summary = Summary {
            kind: stream.then_some("result"),
            session_id: self.session.clone(),
            trace_id: trace,
            turn_id: terminal["turnId"].as_str().map(str::to_owned),
            response,
            usage: Usage::from_payload(&payload["usage"]),
            event_count: self.events,
            projection: Projection::from_snapshot(&subscribed["snapshot"]["projection"]),
        };
        let text = if stream {
            serde_json::to_string(&summary)
        } else {
            serde_json::to_string_pretty(&summary)
        }
        .map_err(|e| Failure::Io(e.to_string()))?;
        self.io
            .stdout
            .write_all(format!("{text}\n").as_bytes())
            .await?;
        self.io.stdout.flush().await?;
        Ok(0)
    }

    async fn write_error(
        &mut self,
        message: &str,
        trace: Option<&str>,
        cause: Option<String>,
    ) -> Result<(), Failure> {
        let mut text = error_line(message, trace);
        if let Some(cause) = cause {
            text.push_str(&format!("Cause: {cause}\n"));
        }
        self.io.stderr.write_all(text.as_bytes()).await?;
        self.io.stderr.flush().await?;
        Ok(())
    }
}

/// Runs `request` against a started runtime actor and returns the exit code.
/// The caller then sends `Eof` and drains `from` until the actor stops.
pub async fn run<O, E>(
    request: Request,
    (to, from): (
        &mpsc::Sender<ClientMsg>,
        &mut mpsc::Receiver<Vec<ServerMsg>>,
    ),
    io: &mut Io<O, E>,
    mut interrupt: Interrupt,
) -> i32
where
    O: AsyncWrite + Unpin,
    E: AsyncWrite + Unpin,
{
    let mut driver = Driver {
        to,
        from,
        pending: VecDeque::new(),
        token: 0,
        io,
        format: request.format,
        session: String::new(),
        submitted: false,
        turn: None,
        trace: None,
        terminal: None,
        events: 0,
    };
    let outcome = async {
        driver.open(&request).await?;
        if let Some(code) = *interrupt.borrow() {
            return Ok(code);
        }
        driver.submit(&request).await?;
        let signalled = driver.finish(&mut interrupt).await?;
        let code = driver.report(&request).await?;
        Ok::<_, Failure>(signalled.unwrap_or(code))
    }
    .await;
    match outcome {
        Ok(code) => code,
        Err(Failure::Request(message) | Failure::Io(message)) => {
            let line = error_line(&message, None);
            let _ = driver.io.stderr.write_all(line.as_bytes()).await;
            let _ = driver.io.stderr.flush().await;
            interrupt.borrow().unwrap_or(1)
        }
    }
}
