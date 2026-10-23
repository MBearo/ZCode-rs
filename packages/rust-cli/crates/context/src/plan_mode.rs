//! 计划模式的 attachment（docs/specs/rust-cli/context.md §13，工作线 L11c）：`plan_mode`（完整、简略、
//! 子代理三种文本）、`plan_mode_reentry`、`plan_mode_exit`。
//!
//! 模式与两个会话标志取 `ContextRequest.mode` / `plan_flags`（会话 actor 是唯一 owner，同一个快照），
//! 计划文件取 kernel-api 的 `PlanFiles`（与 ExitPlanMode、判决链同一来源）。节奏从压缩边界之后的历史
//! 推导；再次进入看 `has_exited`，退出看 `needs_exit_attachment`
//! 或历史（§13.4）。context 不改标志：actor 提交这里产生的 reentry /
//! exit 时清掉对应的标志。

use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::env_files::{FileKind, StatFile};
use rust_cli_kernel_api::message::{AttachmentMessage, Message, UserOrigin};
use rust_cli_kernel_api::orchestration::ContextRequest;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::plan::{self, PlanFiles, PlanModeFlags};
use rust_cli_prompts::texts::{self, render};
use serde_json::{Value, json};

use crate::PromptAssembler;

/// 来源（attachment type）。
pub(crate) const PLAN_MODE: &str = "plan_mode";
pub(crate) const REENTRY: &str = plan::REENTRY_SOURCE;
pub(crate) const EXIT: &str = plan::EXIT_SOURCE;
/// 两次 plan_mode 之间至少隔几次人类输入。
const TURNS_BETWEEN_ATTACHMENTS: usize = 5;
/// 每第几条 plan_mode 是完整提醒。
const FULL_REMINDER_EVERY_N_ATTACHMENTS: usize = 5;

/// 最近一条计划模式 attachment。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Last {
    /// `plan_mode` 或 `plan_mode_reentry`。
    Entered,
    /// `plan_mode_exit`。
    Exited,
}

/// 这一步要发什么（不含 IO，便于单测）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Due {
    Nothing,
    /// 发 `plan_mode`。`reentry`：离开过 plan 模式（会话标志），计划文件存在时先发 reentry。
    Enter {
        full: bool,
        reentry: bool,
    },
    Exit,
}

/// 从后往前读历史：最近一条计划模式 attachment、它之后的人类输入次数（遇到 exit 就停），
/// 以及最近的 exit 之后的 `plan_mode` 条数。
fn trail(history: &[Message]) -> (Option<Last>, usize, usize) {
    let (mut last, mut turns, mut since_exit) = (None, 0, 0);
    for message in history.iter().rev() {
        match message {
            Message::User(user) if last.is_none() && user.origin == UserOrigin::Human => {
                turns += 1;
            }
            Message::Attachment(attachment) => match attachment.source.as_str() {
                PLAN_MODE => {
                    last.get_or_insert(Last::Entered);
                    since_exit += 1;
                }
                REENTRY => {
                    last.get_or_insert(Last::Entered);
                }
                EXIT => {
                    last.get_or_insert(Last::Exited);
                    break;
                }
                _ => {}
            },
            _ => {}
        }
    }
    (last, turns, since_exit)
}

/// 按模式、会话标志与历史决定这一步的计划模式 attachment。
pub(crate) fn due(history: &[Message], mode: PermissionMode, flags: PlanModeFlags) -> Due {
    let (last, turns, since_exit) = trail(history);
    if mode != PermissionMode::Plan {
        // 退出：标志为真，或历史里（最近的退出之后）有 plan_mode / reentry。
        let entered = last == Some(Last::Entered);
        return if flags.needs_exit_attachment || entered {
            Due::Exit
        } else {
            Due::Nothing
        };
    }
    if last == Some(Last::Entered) && turns < TURNS_BETWEEN_ATTACHMENTS {
        return Due::Nothing;
    }
    Due::Enter {
        full: (since_exit + 1) % FULL_REMINDER_EVERY_N_ATTACHMENTS == 1,
        // 再次进入：离开过 plan 模式的标志为真（计划文件是否存在在产生时再看）。
        reentry: flags.has_exited,
    }
}

/// `plan_mode` 的文本：子代理、简略、完整。
fn reminder(full: bool, subagent: bool, path: &str, exists: bool) -> String {
    let info =
        |new: &str, existing: &str| render(if exists { existing } else { new }, &[("path", path)]);
    if subagent {
        let info = info(
            texts::plan_mode_subagent_file_new(),
            texts::plan_mode_subagent_file_exists(),
        );
        return render(texts::plan_mode_subagent(), &[("plan_file_info", &info)]);
    }
    if !full {
        return render(texts::plan_mode_sparse(), &[("path", path)]);
    }
    let info = info(texts::plan_mode_file_new(), texts::plan_mode_file_exists());
    render(texts::plan_mode_full(), &[("plan_file_info", &info)])
}

fn exit_text(path: &str, exists: bool) -> String {
    let file = if exists {
        render(texts::plan_mode_exit_file(), &[("path", path)])
    } else {
        String::new()
    };
    render(texts::plan_mode_exit(), &[("plan_file", &file)])
}

impl PromptAssembler {
    /// 本会话的计划文件（`<工作区>/.zcode/plans/plan-<会话>.md`，子代理是子会话的文件）与它是否存在；
    /// 执行环境答不出路径风格时为 None（不发计划模式 attachment）。
    async fn plan_file(&self, request: &ContextRequest) -> Option<(EnvPath, bool)> {
        let env = self.ports.env.as_ref();
        let facts = match env.path_facts(&request.workspace).await {
            Ok(facts) => facts,
            Err(error) => {
                tracing::warn!(%error, "path facts unavailable; skipping the plan mode attachments");
                return None;
            }
        };
        let path = PlanFiles::in_workspace(&facts, &request.workspace.root, &request.session)?
            .path(request.agent.as_ref());
        // 读得到文件才算存在。
        let stat = env
            .stat(&request.workspace, StatFile { path: path.clone() })
            .await;
        let exists = matches!(stat, Ok(Some(stat)) if stat.kind == FileKind::File);
        Some((path, exists))
    }

    /// 压缩附件里的计划模式提醒：模式是 plan 时总是
    /// 一条完整的 `plan_mode`（不看节奏与再次进入）。以前由压缩后下一步的上下文按“边界之后还没发过”补发，
    /// F10 起当步 attachment 只在输入与工具轮之后产生，所以放进压缩附件（compact.md §3.4）。
    pub(crate) async fn post_compact_plan_mode(
        &self,
        request: &ContextRequest,
    ) -> Option<AttachmentMessage> {
        if request.mode != PermissionMode::Plan {
            return None;
        }
        let (path, exists) = self.plan_file(request).await?;
        let path = path.as_str();
        let subagent = request.agent.is_some();
        let data: Value = json!({
            "reminderType": "full",
            "isSubAgent": subagent,
            "planFilePath": path,
            "planExists": exists,
        });
        let text = reminder(true, subagent, path, exists);
        Some(self.attachment(PLAN_MODE, (text, data)))
    }

    /// 这一步的计划模式 attachment，按产生顺序（reentry 在 plan_mode 前面）。
    pub(crate) async fn plan_mode(
        &self,
        request: &ContextRequest,
        history: &[Message],
    ) -> Vec<AttachmentMessage> {
        let due = due(history, request.mode, request.plan_flags);
        if due == Due::Nothing {
            return Vec::new();
        }
        let Some((path, exists)) = self.plan_file(request).await else {
            return Vec::new();
        };
        let path = path.as_str();
        match due {
            Due::Nothing => Vec::new(),
            Due::Exit => {
                let data = json!({"planFilePath": path, "planExists": exists});
                vec![self.attachment(EXIT, (exit_text(path, exists), data))]
            }
            Due::Enter { full, reentry } => {
                let mut out = Vec::new();
                if reentry && exists {
                    let text = render(texts::plan_mode_reentry(), &[("path", path)]);
                    out.push(self.attachment(REENTRY, (text, json!({"planFilePath": path}))));
                }
                let subagent = request.agent.is_some();
                let data: Value = json!({
                    "reminderType": if full { "full" } else { "sparse" },
                    "isSubAgent": subagent,
                    "planFilePath": path,
                    "planExists": exists,
                });
                let text = reminder(full, subagent, path, exists);
                out.push(self.attachment(PLAN_MODE, (text, data)));
                out
            }
        }
    }
}

#[cfg(test)]
#[path = "plan_mode_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "plan_mode_due_tests.rs"]
mod due_tests;
