//! Background lifecycle of Bash jobs: explicit `run_in_background` and Node's
//! `auto_on_timeout` (a foreground command past its timeout keeps running as a
//! background task instead of being killed). Spec rust-m5-tools §7.
use super::tool_process::run;
use super::tool_shell::{Job, ShellTasks};
use crate::{
    contract::{Event, EventSink},
    domain::background::BackgroundTask,
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{path::Path, path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{Mutex, oneshot, watch};
use tokio_util::sync::CancellationToken;

/// One Bash command about to start.
pub(super) struct Launch {
    pub id: String,
    pub path: PathBuf,
    pub combined: Arc<Mutex<tokio::fs::File>>,
    pub command: String,
    pub description: String,
}

/// Ends the foreground process if the call is dropped before it went to the background.
struct Foreground(Option<CancellationToken>);

impl Drop for Foreground {
    fn drop(&mut self) {
        if let Some(token) = self.0.take() {
            token.cancel();
        }
    }
}

/// Node `isBashAutoBackgroundEligible`.
pub(super) fn auto_eligible(command: &str) -> bool {
    let command = crate::domain::js_string::trim(command);
    !command.is_empty() && command.split_whitespace().next() != Some("sleep")
}

/// The Node `status: "backgrounded"` result of a job.
pub(super) fn backgrounded(launch: &Launch) -> Value {
    json!({"stdout":"","stderr":"","status":"backgrounded","interrupted":false,
        "backgroundTaskId":launch.id,"persistedOutputPath":launch.path,"backgroundedByUser":false})
}

/// The job's end: the terminal task goes to the owner before TaskOutput and
/// TaskStop see the result (same channel keeps the commit first).
async fn finish(
    sink: &EventSink,
    mut task: BackgroundTask,
    tx: &watch::Sender<Option<Value>>,
    result: Result<Value>,
) {
    if let Err(error) = &result
        && error.is::<crate::contract::ProcessCleanupFailure>()
    {
        let _ = sink
            .send(Event::ToolCleanupFailed(format!("{error:#}")))
            .await;
    }
    let result = result.unwrap_or_else(
        |e| json!({"stdout":"","stderr":e.to_string(),"status":"spawn_error","interrupted":false}),
    );
    task.ended_at = Some(super::now());
    task.status = match result["status"].as_str() {
        Some("completed") => "completed",
        Some("cancelled") => "cancelled",
        _ => "failed",
    }
    .into();
    let _ = sink
        .send(Event::Background {
            task,
            committed: None,
        })
        .await;
    let _ = tx.send(Some(result));
}

impl ShellTasks {
    /// Adds a running job of `session` (at most 16 running, 128 kept) and
    /// commits its running task with the owner; on failure no job stays.
    async fn register(
        &self,
        session: &str,
        sink: &EventSink,
        launch: &Launch,
        (token, state): (CancellationToken, watch::Receiver<Option<Value>>),
        cancel: &CancellationToken,
    ) -> Result<BackgroundTask> {
        let task = BackgroundTask {
            id: launch.id.clone(),
            run_id: sink.run_id.clone(),
            title: launch.description.clone(),
            status: "running".into(),
            started_at: super::now(),
            ended_at: None,
            output_file: launch.path.to_string_lossy().into_owned(),
        };
        let job = Arc::new(Job {
            cancel: token,
            state,
            path: launch.path.clone(),
            command: launch.command.clone(),
            description: launch.description.clone(),
        });
        {
            let mut all = self.jobs.lock().await;
            let jobs = all.entry(session.to_owned()).or_default();
            if jobs.values().filter(|j| j.state.borrow().is_none()).count() >= 16 {
                bail!("Background task limit (16) reached");
            }
            if jobs.len() >= 128
                && let Some(old) = jobs
                    .iter()
                    .find(|(_, j)| j.state.borrow().is_some())
                    .map(|(id, _)| id.clone())
            {
                jobs.remove(&old);
            }
            jobs.insert(launch.id.clone(), job);
        }
        let (committed, receipt) = oneshot::channel();
        let registered = async {
            sink.send(Event::Background {
                task: task.clone(),
                committed: Some(committed),
            })
            .await?;
            receipt
                .await
                .context("Background registration was not committed")?;
            Ok::<_, anyhow::Error>(())
        };
        let registered = tokio::select! {_=cancel.cancelled()=>Err(anyhow::anyhow!("Cancelled")),r=registered=>r};
        if let Err(e) = registered {
            // owner 可能已经提交 running、但工具尚未收到回执；取消时也要投递终态，
            // 否则 close/EOF 会永远等待一个从未结束的后台任务。
            let mut terminal = task;
            terminal.status = if cancel.is_cancelled() {
                "cancelled"
            } else {
                "failed"
            }
            .into();
            terminal.ended_at = Some(super::now());
            let _ = sink
                .send(Event::Background {
                    task: terminal,
                    committed: None,
                })
                .await;
            if let Some(jobs) = self.jobs.lock().await.get_mut(session) {
                jobs.remove(&launch.id);
            }
            return Err(e);
        }
        Ok(task)
    }

    /// `run_in_background`: the job is registered, then started.
    pub(super) async fn explicit(
        &self,
        (cwd, session): (&Path, &str),
        sink: &EventSink,
        launch: Launch,
        timeout: Option<Duration>,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let token = CancellationToken::new();
        let (tx, state) = watch::channel(None);
        let task = match self
            .register(session, sink, &launch, (token.clone(), state), cancel)
            .await
        {
            Ok(task) => task,
            Err(error) => {
                let _ = tx.send(Some(json!({"status":"cancelled","interrupted":true})));
                return Err(error);
            }
        };
        if cancel.is_cancelled() {
            token.cancel();
        }
        let (cwd, env, sink) = (cwd.to_owned(), self.env.clone(), sink.clone());
        let result = backgrounded(&launch);
        tokio::spawn(async move {
            let Launch {
                path,
                combined,
                command,
                ..
            } = launch;
            let outcome = run(&cwd, &env, &command, &path, combined, timeout, &token).await;
            finish(&sink, task, &tx, outcome).await;
        });
        Ok(result)
    }

    /// Node `auto_on_timeout`: the command runs in the foreground until
    /// `deadline`; still running then, it becomes a background task.
    pub(super) async fn auto(
        &self,
        (cwd, session): (&Path, &str),
        sink: &EventSink,
        launch: Launch,
        deadline: Duration,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let token = CancellationToken::new();
        let (tx, state) = watch::channel(None);
        let (cwd_owned, env) = (cwd.to_owned(), self.env.clone());
        let (path, combined, command) = (
            launch.path.clone(),
            launch.combined.clone(),
            launch.command.clone(),
        );
        let process_token = token.clone();
        let mut guard = Foreground(Some(token.clone()));
        let mut process = tokio::spawn(async move {
            run(
                &cwd_owned,
                &env,
                &command,
                &path,
                combined,
                None,
                &process_token,
            )
            .await
        });
        tokio::select! {
            joined = &mut process => return joined?,
            _ = cancel.cancelled() => {
                // 前台期间的取消照常结束进程树，结果为 cancelled。
                token.cancel();
                return process.await?;
            }
            _ = tokio::time::sleep(deadline) => {}
        }
        // 超过前台时限：先登记为后台任务，此后本轮取消不再影响该进程。
        guard.0 = None;
        let task = match self
            .register(session, sink, &launch, (token.clone(), state), cancel)
            .await
        {
            Ok(task) => task,
            Err(error) => {
                token.cancel();
                let _ = process.await;
                let _ = tx.send(Some(json!({"status":"cancelled","interrupted":true})));
                return Err(error);
            }
        };
        let sink = sink.clone();
        tokio::spawn(async move {
            let outcome = match process.await {
                Ok(outcome) => outcome,
                Err(error) => Err(error.into()),
            };
            finish(&sink, task, &tx, outcome).await;
        });
        Ok(backgrounded(&launch))
    }
}
