//! Bash and background task results as the model reads them (Node
//! `bash-model-content.ts`, `bash-semantics.ts`, `task-output.ts`,
//! `task-stop.ts`). `data` is the shell adapter's result object.
use crate::domain::{
    js_string,
    persisted_output::{compact_bytes, envelope},
};
use serde_json::{Value, json};

/// Merged output the model sees inline (Node `MAX_INLINE_OUTPUT_BYTES`).
pub(super) const INLINE_BYTES: usize = 30_000;
const PREVIEW_CHARS: usize = 2_000;
/// Node `TASK_OUTPUT_DEFAULT_LENGTH`.
const TASK_OUTPUT_CHARS: usize = 32_000;

/// Node `semanticNonErrorExit`: exit 1 of the last command means "no
/// match", "differs" or "false" for these commands, not a failure.
fn semantic_exit_one(command: &str) -> bool {
    let analysis = zcode_cli_bash_parse::analyze(command);
    if analysis.has_parse_errors || analysis.has_unsupported_syntax {
        return false;
    }
    let Some(last) = analysis.commands.last() else {
        return false;
    };
    let name = if last.name == "git" {
        match git_subcommand(&last.argv) {
            Some("grep") => "grep",
            Some("diff") => "diff",
            _ => "git",
        }
    } else {
        last.name.as_str()
    };
    matches!(
        name,
        "egrep" | "fgrep" | "grep" | "rg" | "find" | "diff" | "test" | "["
    )
}

/// Node `gitSemanticSubcommandName`: first non-option argument; `-C` / `-c` take a value.
fn git_subcommand(argv: &[String]) -> Option<&str> {
    let mut index = 1;
    while let Some(arg) = argv.get(index) {
        if arg.starts_with('-') {
            if arg == "-C" || arg == "-c" {
                index += 1;
            }
            index += 1;
            continue;
        }
        return Some(arg);
    }
    None
}

/// Node `isBashProviderErrorStatus`.
fn provider_error(command: &str, data: &Value) -> bool {
    let Some(code) = data["exitCode"].as_i64() else {
        return false;
    };
    data["status"] == "failed" && code != 0 && !(code == 1 && semantic_exit_one(command))
}

/// `stdout.replace(/^(\s*\n)+/, "").trimEnd()`.
fn stdout_text(stdout: &str) -> &str {
    let mut start = 0;
    for (at, c) in stdout.char_indices() {
        if !js_string::is_space(c) {
            break;
        }
        if c == '\n' {
            start = at + 1;
        }
    }
    stdout[start..].trim_end_matches(js_string::is_space)
}

/// The model content of a Bash result and whether the provider sees it as an error.
pub(super) fn bash_content(command: &str, data: &Value) -> (String, bool) {
    let error = provider_error(command, data);
    let interrupted = data["interrupted"] == true;
    let backgrounded = data["status"] == "backgrounded";
    let path = data["persistedOutputPath"].as_str();
    let mut stdout = stdout_text(data["stdout"].as_str().unwrap_or("")).to_owned();
    let total = data["persistedOutputSize"].as_u64().unwrap_or(0);
    if let Some(path) = path.filter(|_| !backgrounded && total > INLINE_BYTES as u64) {
        stdout = envelope(&stdout, total, path, PREVIEW_CHARS, compact_bytes);
    }
    let mut stderr = js_string::trim(data["stderr"].as_str().unwrap_or("")).to_owned();
    if interrupted {
        if !stderr.is_empty() {
            stderr.push('\n');
        }
        stderr.push_str("<error>Command was aborted before completion</error>");
    }
    let background = data["backgroundTaskId"].as_str().map_or(String::new(), |id| {
        let written = path.map_or(String::new(), |p| format!(" Output is being written to: {p}."));
        let hint = if written.is_empty() {
            ""
        } else {
            " To check interim output, use Read on that file path."
        };
        format!(
            "Command running in background with ID: {id}.{written} You will be notified when it completes.{hint}"
        )
    });
    let exit = if error {
        format!("Exit code {}", data["exitCode"])
    } else {
        String::new()
    };
    let text = [exit, stdout, stderr, background]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (text, error || interrupted)
}

/// Node `formatTimeoutDuration`.
pub(super) fn duration(ms: u64) -> String {
    let unit = |value: u64, per: u64| {
        if value.is_multiple_of(per) {
            (value / per).to_string()
        } else {
            let tenths = (u128::from(value) * 10 + u128::from(per) / 2) / u128::from(per);
            let text = format!("{}.{}", tenths / 10, tenths % 10);
            text.strip_suffix(".0").map(str::to_owned).unwrap_or(text)
        }
    };
    match ms {
        m if m < 1_000 => format!("{m}ms"),
        m if m < 60_000 => format!("{}s", unit(m, 1_000)),
        m if m < 3_600_000 => format!("{}m", unit(m, 60_000)),
        m => format!("{}h", unit(m, 3_600_000)),
    }
}

/// Node `formatTaskOutputModelContent` for a local Bash task.
pub(super) fn task_output_content(retrieval: &str, task: &Value) -> String {
    let mut blocks = vec![format!("<retrieval_status>{retrieval}</retrieval_status>")];
    blocks.push(format!(
        "<task_id>{}</task_id>",
        task["task_id"].as_str().unwrap_or("")
    ));
    blocks.push(format!(
        "<task_type>{}</task_type>",
        task["task_type"].as_str().unwrap_or("")
    ));
    blocks.push(format!(
        "<status>{}</status>",
        task["status"].as_str().unwrap_or("")
    ));
    if let Some(code) = task["exitCode"].as_i64() {
        blocks.push(format!("<exit_code>{code}</exit_code>"));
    }
    let output = task["output"].as_str().unwrap_or("");
    if !js_string::trim(output).is_empty() {
        let content = match task["outputFile"].as_str() {
            Some(path) => truncated_task_output(output, path),
            None => output.to_owned(),
        };
        let content = content.trim_end_matches(js_string::is_space);
        blocks.push(format!("<output>\n{content}\n</output>"));
    }
    blocks.join("\n\n")
}

/// Node `truncateTaskOutput`: keep the UTF-16 tail after a pointer to the file.
fn truncated_task_output(output: &str, path: &str) -> String {
    let units: Vec<u16> = output.encode_utf16().collect();
    if units.len() <= TASK_OUTPUT_CHARS {
        return output.to_owned();
    }
    let prefix = format!("[Truncated. Full output: {path}]\n\n");
    let keep = TASK_OUTPUT_CHARS.saturating_sub(prefix.encode_utf16().count());
    let tail = String::from_utf16_lossy(&units[units.len() - keep..]);
    prefix + &tail
}

/// Node TaskStop result: compact JSON in schema key order.
pub(super) fn task_stop_content(id: &str, command: &str) -> (String, Value) {
    let message = format!("Successfully stopped task: {id} ({command})");
    let text = format!(
        "{{\"message\":{},\"task_id\":{},\"task_type\":\"local_bash\",\"command\":{}}}",
        json!(message),
        json!(id),
        json!(command)
    );
    let data =
        json!({"message": message, "task_id": id, "task_type": "local_bash", "command": command});
    (text, data)
}

#[cfg(test)]
#[path = "bash_output_tests.rs"]
mod tests;
