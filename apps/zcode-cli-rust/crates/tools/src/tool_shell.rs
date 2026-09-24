use super::tool_process::run;
use super::tools::{boolean, keys, string};
use crate::{
    contract::{Event, EventSink, ToolError, ToolOutput},
    domain::background::BackgroundTask,
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Mutex, oneshot, watch};
use tokio_util::sync::CancellationToken;
#[path = "task_tools.rs"]
mod task;

struct Job {
    cancel: CancellationToken,
    state: watch::Receiver<Option<Value>>,
    path: PathBuf,
    command: String,
    description: String,
}
pub struct ShellTasks {
    jobs: Mutex<HashMap<String, HashMap<String, Arc<Job>>>>,
    /// Complete child environment (Node `buildExecutionEnv`).
    env: Arc<[(String, String)]>,
}
impl ShellTasks {
    pub fn new(env: Arc<[(String, String)]>) -> Self {
        Self {
            jobs: Mutex::default(),
            env,
        }
    }
    pub async fn call(
        &self,
        paths: (&Path, &Path),
        session: &str,
        name: &str,
        args: &Value,
        sink: Option<&EventSink>,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput> {
        let (cwd, artifacts) = paths;
        match name {
            "Bash" => {
                self.start(cwd, artifacts, session, args, sink, cancel)
                    .await
            }
            "TaskOutput" | "TaskStop" => {
                keys(
                    args,
                    if name == "TaskOutput" {
                        &["task_id", "block", "timeout"]
                    } else {
                        &["task_id", "shell_id"]
                    },
                )?;
                let id = args["task_id"]
                    .as_str()
                    .or_else(|| args["shell_id"].as_str())
                    .filter(|id| !id.is_empty());
                let stop = name == "TaskStop";
                let Some(id) = id else {
                    if stop {
                        bail!("Missing required parameter: task_id");
                    }
                    return Err(ToolError::handler(1, "Task ID is required"));
                };
                let job = self
                    .jobs
                    .lock()
                    .await
                    .get(session)
                    .and_then(|jobs| jobs.get(id))
                    .cloned();
                let Some(job) = job else {
                    // Node：TaskOutput 在 validateInput 返回处理器失败，TaskStop 抛出错误。
                    let message = format!("No task found with ID: {id}");
                    if stop {
                        bail!("{message}");
                    }
                    return Err(ToolError::handler(2, message));
                };
                if stop {
                    return self.stop(id, &job, cancel).await;
                }
                self.task_output(id, &job, args, cancel).await
            }
            _ => bail!("Unsupported shell tool"),
        }
    }
    async fn start(
        &self,
        cwd: &Path,
        artifacts: &Path,
        session: &str,
        args: &Value,
        sink: Option<&EventSink>,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput> {
        keys(
            args,
            &[
                "command",
                "description",
                "timeout",
                "run_in_background",
                "dangerouslyDisableSandbox",
            ],
        )?;
        let command = string(args, "command")?.to_owned();
        if crate::domain::js_string::trim(&command).is_empty() {
            // Node：空命令返回空结果（由通用占位显示 "(Bash completed with no output)"）。
            return Ok(ToolOutput::text(String::new()));
        }
        let background = boolean(args, "run_in_background", false)?;
        boolean(args, "dangerouslyDisableSandbox", false)?;
        let description = args
            .get("description")
            .map(|_| string(args, "description"))
            .transpose()?
            .unwrap_or(&command)
            .to_owned();
        let timeout = match args.get("timeout") {
            None if background => None,
            value => {
                let n = match value {
                    None => 120000.0,
                    Some(v) => v
                        .as_f64()
                        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                        .context("Invalid Bash timeout")?,
                };
                if !n.is_finite() || n < 0.0 {
                    bail!("Invalid Bash timeout");
                }
                Some(Duration::from_millis(if n == 0.0 {
                    120000
                } else {
                    (n as u64).min(600000)
                }))
            }
        };
        tokio::fs::create_dir_all(artifacts).await?;
        let id = super::id();
        let path = artifacts.join(format!("{id}.output"));
        let combined = Arc::new(Mutex::new(tokio::fs::File::create(&path).await?));
        if !background {
            let data = run(cwd, &self.env, &command, &path, combined, timeout, cancel).await?;
            return Ok(shell_output(&command, data));
        }
        let sink = sink
            .context("Background execution requires a session owner")?
            .clone();
        let task = BackgroundTask {
            id: id.clone(),
            run_id: sink.run_id.clone(),
            title: description.clone(),
            status: "running".into(),
            started_at: super::now(),
            ended_at: None,
            output_file: path.to_string_lossy().into_owned(),
        };
        let token = CancellationToken::new();
        let (tx, state) = watch::channel(None);
        let job = Arc::new(Job {
            cancel: token.clone(),
            state,
            path: path.clone(),
            command: command.clone(),
            description,
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
            jobs.insert(id.clone(), job);
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
            // 否则 close/EOF 会永远等待一个从未 spawn 的后台任务。
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
            let _ = tx.send(Some(json!({"status":"cancelled","interrupted":true})));
            self.jobs.lock().await.get_mut(session).unwrap().remove(&id);
            return Err(e);
        }
        if cancel.is_cancelled() {
            token.cancel();
        }
        let cwd = cwd.to_owned();
        let command_copy = command.clone();
        let path_copy = path.clone();
        let env = self.env.clone();
        tokio::spawn(async move {
            let result = run(
                &cwd,
                &env,
                &command_copy,
                &path_copy,
                combined,
                timeout,
                &token,
            )
            .await;
            if let Err(error) = &result
                && error.is::<crate::contract::ProcessCleanupFailure>()
            {
                let _ = sink
                    .send(Event::ToolCleanupFailed(format!("{error:#}")))
                    .await;
            }
            let result = result.unwrap_or_else(|e|json!({"stdout":"","stderr":e.to_string(),"status":"spawn_error","interrupted":false}));
            let mut task = task;
            task.ended_at = Some(super::now());
            task.status = match result["status"].as_str() {
                Some("completed") => "completed",
                Some("cancelled") => "cancelled",
                _ => "failed",
            }
            .into();
            // 先排入 owner 的终态事件，再允许 TaskOutput/TaskStop 返回；同一通道保持提交先于工具结果。
            let _ = sink
                .send(Event::Background {
                    task,
                    committed: None,
                })
                .await;
            let _ = tx.send(Some(result));
        });
        Ok(shell_output(
            &command,
            json!({"stdout":"","stderr":"","status":"backgrounded","interrupted":false,"backgroundTaskId":id,"persistedOutputPath":path,"backgroundedByUser":false}),
        ))
    }
    pub async fn cancel(&self, session: &str, id: Option<&str>) -> Result<()> {
        let all = self.jobs.lock().await;
        if let Some(id) = id {
            all.get(session)
                .and_then(|v| v.get(id))
                .context("Background task unavailable")?
                .cancel
                .cancel();
        } else if let Some(jobs) = all.get(session) {
            for job in jobs.values() {
                job.cancel.cancel();
            }
        }
        Ok(())
    }
    pub async fn shutdown(&self) -> Result<()> {
        let jobs: Vec<_> = self
            .jobs
            .lock()
            .await
            .values()
            .flat_map(|jobs| jobs.values().cloned())
            .collect();
        for job in &jobs {
            job.cancel.cancel();
        }
        for job in jobs {
            let mut rx = job.state.clone();
            if rx.borrow().is_none() {
                let _ = rx.wait_for(|v| v.is_some()).await;
            }
        }
        Ok(())
    }
    pub async fn close_session(&self, session: &str) -> Result<()> {
        let jobs = self.jobs.lock().await.remove(session).unwrap_or_default();
        for job in jobs.values() {
            job.cancel.cancel();
        }
        for job in jobs.values() {
            let mut state = job.state.clone();
            if state.borrow().is_none() {
                state.wait_for(|result| result.is_some()).await?;
            }
        }
        Ok(())
    }
}
/// Node `formatBashModelContent`; `failed` is Node's provider `is_error`.
fn shell_output(command: &str, data: Value) -> ToolOutput {
    let (content, failed) = super::bash_output::bash_content(command, &data);
    let mut output = ToolOutput::new(content, data);
    output.failed = failed;
    output
}
