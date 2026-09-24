//! TaskOutput and TaskStop over a session's background Bash jobs (Node
//! `task-output.ts`, `task-output-bash.ts`, `task-output-projection.ts`, `task-stop.ts`).
use super::{Job, ShellTasks};
use crate::bash_output::{INLINE_BYTES, task_output_content, task_stop_content};
use crate::contract::ToolOutput;
use crate::tools::{boolean, truncate_utf8, uint};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::sync::CancellationToken;

/// Node reads at most the last 8 MiB of a finished task's output file.
const TAIL_BYTES: u64 = 8 * 1024 * 1024;
const DISPLAY_PREVIEW_BYTES: usize = 1800;

/// Node runtime task status of a Bash job.
fn node_status(result: Option<&Value>) -> &'static str {
    match result.map(|r| r["status"].as_str()) {
        None => "running",
        Some(Some("completed")) => "completed",
        Some(Some("cancelled")) => "killed",
        Some(_) => "failed",
    }
}

/// Node `readTaskOutputFileSnapshot`: the tail, with the omitted size first.
async fn read_tail(path: &Path) -> Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let size = file.metadata().await?.len();
    let omitted = size.saturating_sub(TAIL_BYTES);
    file.seek(std::io::SeekFrom::Start(omitted)).await?;
    let mut bytes = vec![];
    file.read_to_end(&mut bytes).await?;
    let content = String::from_utf8_lossy(&bytes);
    if omitted == 0 {
        return Ok(content.into_owned());
    }
    let kb = (omitted + 512) / 1024;
    Ok(format!("[{kb}KB of earlier output omitted]\n{content}"))
}

impl ShellTasks {
    /// Node TaskStop is strict: a finished task is an error.
    pub(super) async fn stop(
        &self,
        id: &str,
        job: &Job,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput> {
        let finished = job.state.borrow().clone();
        if finished.is_some() {
            bail!(
                "Task {id} is not running (status: {})",
                node_status(finished.as_ref())
            );
        }
        job.cancel.cancel();
        let mut state = job.state.clone();
        tokio::select! {
            _ = cancel.cancelled() => bail!("Cancelled"),
            result = state.wait_for(|v| v.is_some()) => { result?; }
        }
        let (content, data) = task_stop_content(id, &job.command);
        let mut output = ToolOutput::new(content, data);
        output.display = Some(
            json!({"kind": "task_stop", "taskId": id, "taskType": "bash",
            "command": job.command, "message": output.data["message"]}),
        );
        Ok(output)
    }

    /// Node TaskOutput: a running task shows the head of its output, a
    /// finished one the tail.
    pub(super) async fn task_output(
        &self,
        id: &str,
        job: &Job,
        args: &Value,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput> {
        let timeout = uint(args, "timeout", 30000)?;
        if timeout > 600000 {
            bail!("TaskOutput timeout exceeds 600000 ms");
        }
        let mut state = job.state.clone();
        let mut retrieval = "success";
        if state.borrow().is_none() {
            if boolean(args, "block", true)? {
                let wait = tokio::time::timeout(
                    Duration::from_millis(timeout),
                    state.wait_for(|s| s.is_some()),
                );
                tokio::select! {
                    _ = cancel.cancelled() => bail!("Cancelled"),
                    result = wait => match result {
                        Ok(r) => { r?; }
                        Err(_) => retrieval = "timeout",
                    },
                }
            } else {
                retrieval = "not_ready";
            }
        }
        let result = state.borrow().clone();
        let status = node_status(result.as_ref());
        let output = if result.is_none() {
            crate::tool_process::read_head(&job.path, INLINE_BYTES).await?
        } else {
            read_tail(&job.path).await?
        };
        let task = json!({"task_id": id, "task_type": "local_bash", "status": status,
            "description": job.description, "output": output,
            "exitCode": result.as_ref().and_then(|r| r["exitCode"].as_i64()), "outputFile": job.path});
        let content = task_output_content(retrieval, &task);
        let mut preview = output.clone();
        truncate_utf8(&mut preview, DISPLAY_PREVIEW_BYTES);
        let mut display =
            json!({"kind": "task_output", "retrievalStatus": retrieval, "taskStatus": status});
        if !preview.is_empty() {
            display["output"] = preview.into();
        }
        if output.len() > DISPLAY_PREVIEW_BYTES {
            display["truncated"] = true.into();
        }
        let mut result = ToolOutput::new(
            content,
            json!({"retrieval_status": retrieval, "task": task}),
        );
        result.display = Some(display);
        Ok(result)
    }
}
