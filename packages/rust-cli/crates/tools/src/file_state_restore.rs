//! 冷恢复时重建 readFileState（tools-files.md §4）。
//!
//! 从会话历史里找 Read（整份读，没有 offset / limit）、Write、Edit 的调用与结果，按时间顺序后者覆盖前者：
//! - Read：取 tool_result 的文本，去掉 `<system-reminder>` 与行号前缀；重复读取的提示跳过；
//! - Write：取调用里的 content；
//! - Edit：结果不含全文，现在从执行环境读一次，记下当前版本。
//!
//! 历史里没有版本，Read / Write 的记录用 [`Baseline::Content`]：之后按内容判断是否被改过。
//!
//! 写了 `machine` 的调用（R3，remote-exec.md §20.7.3）归到那台机器：只认绝对路径（恢复时不知道那台机器的工作目录），
//! Edit 不重读（那台机器可能没连上），暂定。

use std::collections::HashMap;

use regex::Regex;
use rust_cli_kernel_api::env::{EnvPath, ReadFile, WorkspaceRef};
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::env_registry::LOCAL_MACHINE;
use rust_cli_kernel_api::ids::{SessionId, ToolUseId};
use rust_cli_kernel_api::machine::Files;
use rust_cli_kernel_api::machine_param::named_in;
use rust_cli_kernel_api::message::{ContentBlock, Message, ToolResultContent};
use serde_json::Value;

use crate::encoding::{decode, normalize};
use crate::file_state::{Baseline, FileKey, FileState, ReadFileState};
use crate::{edit, read, texts, write};

enum Pending {
    Read(FileKey),
    Write(FileKey, String),
    Edit(String),
}

fn field<'a>(input: &'a Value, name: &str) -> Option<&'a str> {
    input
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

fn calls(messages: &[Message], root: &EnvPath) -> HashMap<ToolUseId, Pending> {
    let style = PathStyle::infer(root.as_str());
    let mut found = HashMap::new();
    for message in messages {
        let Message::Assistant(assistant) = message else {
            continue;
        };
        for block in &assistant.content {
            let ContentBlock::ToolUse { id, name, input } = block else {
                continue;
            };
            let machine = named_in(input);
            let Some(raw) = field(input, "file_path") else {
                continue;
            };
            let path = match machine {
                // 与工具记下的键同一规则（F14）。
                None => style.resolve_user_path(root.as_str(), raw, None),
                Some(_) if PathStyle::infer(raw).is_absolute(raw) => raw.to_owned(),
                Some(_) => continue,
            };
            let key = FileKey::new(machine.unwrap_or(LOCAL_MACHINE), &path);
            let pending = match name.as_str() {
                read::NAME if input.get("offset").is_none() && input.get("limit").is_none() => {
                    Pending::Read(key)
                }
                write::NAME => match field(input, "content") {
                    Some(content) => Pending::Write(key, content.to_owned()),
                    None => continue,
                },
                edit::NAME if machine.is_none() => Pending::Edit(path),
                _ => continue,
            };
            found.insert(id.clone(), pending);
        }
    }
    found
}

/// Read 结果去掉提醒与行号后的文件内容。
fn file_text(result: &str) -> String {
    let reminders = Regex::new(r"(?s)<system-reminder>.*?</system-reminder>").ok();
    let prefix = Regex::new(r"^\s*\d+[\u{2192}\t:](.*)$").ok();
    let cleaned = reminders.map_or_else(
        || result.to_owned(),
        |pattern| pattern.replace_all(result, "").into_owned(),
    );
    cleaned
        .split('\n')
        .map(|line| {
            prefix
                .as_ref()
                .and_then(|pattern| pattern.captures(line))
                .and_then(|captures| captures.get(1))
                .map_or(line, |found| found.as_str())
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

fn entry(content: String, baseline: Baseline) -> FileState {
    FileState {
        content,
        baseline,
        offset: None,
        limit: None,
        partial: false,
        not_in_context: false,
    }
}

/// 用会话历史重建这个会话的 readFileState（会话 owner 在冷加载后调用一次）。
pub async fn restore_file_state(
    state: &ReadFileState,
    session: &SessionId,
    messages: &[Message],
    files: &dyn Files,
    workspace: &WorkspaceRef,
) {
    // 相对路径按工作区根解析，不按会话的当前目录：冷恢复的基准是启动时的目录，
    // 不是写下这条消息时的当前目录（tools-files.md §3.3、§4）。
    let calls = calls(messages, &workspace.root);
    let mut entries: Vec<(FileKey, FileState)> = Vec::new();
    for message in messages {
        let Message::User(user) = message else {
            continue;
        };
        for block in &user.content {
            let ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } = block
            else {
                continue;
            };
            match calls.get(tool_use_id) {
                Some(Pending::Read(key)) => {
                    let [ToolResultContent::Text { text }] = content.as_slice() else {
                        continue;
                    };
                    if text.starts_with(texts::FILE_UNCHANGED) {
                        continue;
                    }
                    entries.push((key.clone(), entry(file_text(text), Baseline::Content)));
                }
                Some(Pending::Write(key, content)) => {
                    entries.push((key.clone(), entry(normalize(content), Baseline::Content)));
                }
                Some(Pending::Edit(path)) if !is_error => {
                    let request = ReadFile {
                        path: EnvPath::new(path.clone()),
                    };
                    if let Ok(file) = files.read_file(workspace, request).await {
                        let text = decode(&file.bytes).content;
                        let key = FileKey::new(LOCAL_MACHINE, path);
                        entries.push((key, entry(text, Baseline::Version(file.version))));
                    }
                }
                Some(Pending::Edit(_)) | None => {}
            }
        }
    }
    state.replace(session, entries);
}

#[cfg(test)]
#[path = "file_state_restore_tests.rs"]
mod tests;
