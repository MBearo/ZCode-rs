//! nested memory 去重用的“触及过与发过的路径”：按会话缓存、只处理新增的消息（context.md §11.6，修复线 F6，
//! 审查 R5-6）。
//!
//! 性能修复：以前每一步（只要上一步有 Read / Write / Edit）都从头扫一遍压缩边界后的全部历史，配对全部工具调用与结果。
//! 这里缓存上一次的计算结果和它覆盖的历史前缀；这一步的历史以它为前缀时只处理新增的消息，否则从头算。缓存只是历史的
//! 计算结果（历史是唯一来源，与 readFileState 冷恢复同源），不另存会话状态，丢了可以重算；会话关闭时与规则缓存一起释放。
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, PoisonError};

use rust_cli_kernel_api::ids::{MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::message::{ContentBlock, Message};
use serde_json::Value;

use super::{SOURCE, TRIGGERING};
use crate::rules::Scope;
use rust_cli_kernel_api::env_files::PathStyle;

/// 全部会话的缓存。
#[derive(Default)]
pub(crate) struct Seen(Mutex<HashMap<SessionId, Tracked>>);

/// 一个会话的计算结果与它覆盖的历史前缀。
#[derive(Default)]
struct Tracked {
    /// 覆盖的前缀：条数、第一条与最后一条消息的 id。
    len: usize,
    first: Option<MessageId>,
    last: Option<MessageId>,
    /// 解析路径用的参数：换了就从头算。
    base: Option<(String, Option<String>, PathStyle)>,
    /// 触发类工具调用的路径键（同一 id 可能出现在几个块里，各算一次）。
    uses: HashMap<ToolUseId, Vec<String>>,
    answered: HashSet<ToolUseId>,
    failed: HashSet<ToolUseId>,
    /// 成功的触发调用按路径键计数（有结果、没有失败的结果）。
    touched: HashMap<String, usize>,
    /// 作为 `nested_memory` 发过的路径键。
    announced: HashSet<String>,
}

impl Tracked {
    fn ok(&self, id: &ToolUseId) -> bool {
        self.answered.contains(id) && !self.failed.contains(id)
    }

    fn count(&mut self, id: &ToolUseId, delta: isize) {
        for key in self.uses.get(id).cloned().unwrap_or_default() {
            let count = self.touched.entry(key.clone()).or_default();
            *count = count.saturating_add_signed(delta);
            if *count == 0 {
                self.touched.remove(&key);
            }
        }
    }

    /// 前缀对得上：历史以覆盖过的部分开头，解析参数也没变。
    fn covers(&self, history: &[Message], base: &(String, Option<String>, PathStyle)) -> bool {
        self.base.as_ref() == Some(base)
            && history.len() >= self.len
            && (self.len == 0
                || (Some(history[0].id()) == self.first.as_ref()
                    && Some(history[self.len - 1].id()) == self.last.as_ref()))
    }

    fn add(&mut self, scope: &Scope<'_>, message: &Message) {
        match message {
            Message::User(user) => {
                for block in &user.content {
                    let ContentBlock::ToolResult {
                        tool_use_id,
                        is_error,
                        ..
                    } = block
                    else {
                        continue;
                    };
                    let before = self.ok(tool_use_id);
                    self.answered.insert(tool_use_id.clone());
                    if *is_error {
                        self.failed.insert(tool_use_id.clone());
                    }
                    match (before, self.ok(tool_use_id)) {
                        (false, true) => self.count(tool_use_id, 1),
                        (true, false) => self.count(tool_use_id, -1),
                        _ => {}
                    }
                }
            }
            Message::Assistant(assistant) => {
                for block in &assistant.content {
                    let ContentBlock::ToolUse { id, name, input } = block else {
                        continue;
                    };
                    let path = input.get("file_path").and_then(Value::as_str);
                    let Some(path) = path.filter(|path| !path.is_empty()) else {
                        continue;
                    };
                    if !TRIGGERING.contains(&name.as_str()) {
                        continue;
                    }
                    let key = scope.style.key(&scope.resolve(path));
                    self.uses.entry(id.clone()).or_default().push(key.clone());
                    if self.ok(id) {
                        *self.touched.entry(key).or_default() += 1;
                    }
                }
            }
            Message::Attachment(attachment) if attachment.source == SOURCE => {
                if let Some(path) = attachment.data.get("path").and_then(Value::as_str) {
                    self.announced.insert(scope.style.key(path));
                }
            }
            Message::Attachment(_) | Message::System(_) => {}
        }
    }
}

impl Seen {
    /// 压缩边界后被 Read / Write / Edit 成功触及过、或已经作为 `nested_memory` 发过的路径键（[`super::announce`]）。
    pub(crate) fn known(
        &self,
        session: &SessionId,
        scope: &Scope<'_>,
        history: &[Message],
    ) -> HashSet<String> {
        let mut sessions = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let tracked = sessions.entry(session.clone()).or_default();
        let base = (scope.cwd.clone(), scope.home.clone(), scope.style);
        if !tracked.covers(history, &base) {
            *tracked = Tracked {
                base: Some(base),
                ..Tracked::default()
            };
        }
        for message in &history[tracked.len..] {
            tracked.add(scope, message);
        }
        tracked.len = history.len();
        tracked.first = history.first().map(|message| message.id().clone());
        tracked.last = history.last().map(|message| message.id().clone());
        tracked
            .touched
            .keys()
            .chain(&tracked.announced)
            .cloned()
            .collect()
    }

    /// 会话关闭：释放它的缓存。
    pub(crate) fn forget(&self, session: &SessionId) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(session);
    }
}
