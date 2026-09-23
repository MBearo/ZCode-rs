use super::tool_process::{INLINE, run};
use super::tools::{boolean, keys, string, truncate_utf8, uint};
use crate::{
    contract::{Event, EventSink, ToolOutput},
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
use tokio::{
    io::AsyncReadExt,
    sync::{Mutex, oneshot, watch},
};
use tokio_util::sync::CancellationToken;
struct Job {
    cancel: CancellationToken,
    state: watch::Receiver<Option<Value>>,
    path: PathBuf,
    command: String,
    description: String,
}
#[derive(Default)]
pub struct ShellTasks {
    jobs: Mutex<HashMap<String, HashMap<String, Arc<Job>>>>,
}
impl ShellTasks {
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
                    .context("task_id required")?;
                let job=self.jobs.lock().await.get(session).and_then(|jobs|jobs.get(id)).cloned().context("Task unavailable in this session (tasks are not restarted after process recovery)")?;
                let mut state = job.state.clone();
                if name == "TaskStop" {
                    job.cancel.cancel();
                    if state.borrow().is_none() {
                        tokio::select! {_=cancel.cancelled()=>bail!("Cancelled"),r=state.wait_for(|v|v.is_some())=>{r?;}}
                    }
                    let message = format!("Task {id} stopped");
                    let data = json!({"message":message,"task_id":id,"task_type":"bash","command":job.command});
                    return Ok(ToolOutput {
                        failed: false,
                        content: message.clone(),
                        display: Some(
                            json!({"kind":"task_stop","taskId":id,"taskType":"bash","command":job.command,"message":message}),
                        ),
                        data,
                    });
                }
                let timeout = uint(args, "timeout", 30000)?;
                if timeout > 600000 {
                    bail!("TaskOutput timeout exceeds 600000 ms");
                }
                let mut retrieval = "success";
                if state.borrow().is_none() {
                    if boolean(args, "block", true)? {
                        tokio::select! {
                            _=cancel.cancelled()=>bail!("Cancelled"),
                            result=tokio::time::timeout(Duration::from_millis(timeout),state.wait_for(|s|s.is_some()))=>{match result{Ok(r)=>{r?;},Err(_)=>retrieval="timeout"}},
                        }
                    } else {
                        retrieval = "not_ready";
                    }
                }
                let final_result = state.borrow().clone();
                let mut file = tokio::fs::File::open(&job.path).await?;
                let mut bytes = vec![];
                (&mut file)
                    .take(INLINE as u64)
                    .read_to_end(&mut bytes)
                    .await?;
                let mut output = String::from_utf8_lossy(&bytes).into_owned();
                if file.metadata().await?.len() > INLINE as u64 {
                    output.push_str(&format!(
                        "\n[output truncated; Read {} with offset/limit]",
                        job.path.display()
                    ));
                }
                let status = final_result
                    .as_ref()
                    .map(|v| match v["status"].as_str() {
                        Some("completed") => "completed",
                        Some("cancelled") => "killed",
                        _ => "failed",
                    })
                    .unwrap_or("running");
                let data = json!({"retrieval_status":retrieval,"task":{"task_id":id,"task_type":"bash","status":status,"description":job.description,"output":output,"exitCode":final_result.as_ref().and_then(|v|v["exitCode"].as_i64()),"outputFile":job.path}});
                let mut preview = output.clone();
                truncate_utf8(&mut preview, 1800);
                let mut display =
                    json!({"kind":"task_output","retrievalStatus":retrieval,"taskStatus":status});
                if !preview.is_empty() {
                    display["output"] = preview.into();
                }
                if output.len() > 1800 {
                    display["truncated"] = true.into();
                }
                Ok(ToolOutput {
                    failed: false,
                    content: serde_json::to_string(&data)?,
                    data,
                    display: Some(display),
                })
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
        if command.trim().is_empty() {
            bail!("command must not be empty");
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
            let data = run(cwd, &command, &path, combined, timeout, cancel).await?;
            return Ok(shell_output(data));
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
        tokio::spawn(async move {
            let result = run(&cwd, &command_copy, &path_copy, combined, timeout, &token).await;
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
fn shell_output(data: Value) -> ToolOutput {
    let failed = matches!(
        data["status"].as_str(),
        Some("failed" | "timed_out" | "cancelled" | "spawn_error")
    );
    let mut output = ToolOutput::new(serde_json::to_string(&data).unwrap(), data);
    output.failed = failed;
    output
}
