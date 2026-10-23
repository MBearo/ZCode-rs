//! 会话工作目录的修改（docs/specs/rust-cli/tools-more.md §11.4）。工作目录只有会话 actor 一个 owner：
//! 工具（EnterWorktree / ExitWorktree）经 `Interactions::set_workspace` 请求修改，`/cd` 经
//! `Command::ChangeDirectory`；actor 先把执行环境里这个会话的 shell cwd 设到新目录，成功后才改内存里的
//! `meta.workspace` 并发 `WorkspaceChanged`（前端的工作区只从这里与快照取）。主循环经
//! `RunSink::current_workspace` 读这里的值，同一回合里后续的工具调用立即在新目录。
//!
//! 不落库：resume 回到会话创建时的目录（tools-more.md D23）。

use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::ids::CommandId;
use rust_cli_kernel_api::manage::DirectoryOutcome;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::orchestration::OrchestrationError;
use rust_cli_kernel_api::session_api::{Ack, KernelEvent};
use rust_cli_kernel_api::store::Fact;

use crate::actor::{Actor, accepted, rejected};

impl Actor {
    /// 修复（R1-3）：以前这里只改 `meta.workspace`，不发事件——EnterWorktree / ExitWorktree 之后前端不知道，
    /// 只好自存一份工作区（只有 `/cd` 更新它），`/diff`、`@` 补全、`/export`、`/permissions` 用旧目录。
    pub(crate) async fn change_workspace(
        &mut self,
        root: EnvPath,
    ) -> Result<(), OrchestrationError> {
        self.deps
            .env
            .set_shell_cwd(&self.meta.id, root.clone())
            .await
            .map_err(|error| OrchestrationError::Failed {
                message: error.to_string(),
            })?;
        self.meta.workspace.root = root;
        self.log.append(KernelEvent::WorkspaceChanged {
            workspace: self.meta.workspace.clone(),
        });
        Ok(())
    }

    /// `Command::ChangeDirectory`（`/cd`，tui-manage.md §7.1）：按自己的工作目录判定，结果是类型化的
    /// [`DirectoryOutcome`]；运行中按固定的文字拒绝。
    ///
    /// 修复（R1-3）：以前 runtime 按前端给的工作区（可能过时的副本）判断“已经在这里”，复制整份 `view().messages`
    /// 去算新目录的规则文件，再按回执文字的前缀分辨“运行中”。现在判定、说明与规则文件都在 actor 里，规则文件经
    /// `DirectoryRules` 用 actor 自己的历史算（不复制）。
    pub(crate) async fn change_directory(&mut self, command: CommandId, root: EnvPath) -> Ack {
        if self.busy() {
            return rejected(BUSY);
        }
        // 缺陷修复（工作线 F57）：以前按字符串 `==`，没有会话时 runtime 却不分大小写，Windows 上 `/cd c:\work` 回到
        // `C:\Work` 被当成换目录。两处统一用 kernel-api `env_path` 的键（规范化；Windows 上不分大小写）。
        let current = self.meta.workspace.root.as_str();
        let outcome = if PathStyle::infer(current).same(root.as_str(), current) {
            DirectoryOutcome::Unchanged
        } else {
            let moved = WorkspaceRef {
                environment: self.meta.workspace.environment.clone(),
                root: root.clone(),
            };
            // 说明文字与新目录的规则文件之间空一行。
            let mut note = rust_cli_prompts::commands::cd_moved(root.as_str());
            if let Some(rules) = self
                .deps
                .directory_rules
                .directory_rules(&moved, &self.messages)
                .await
            {
                note.push_str("\n\n");
                note.push_str(&rules);
            }
            if self.change_workspace(root).await.is_err() {
                DirectoryOutcome::Failed {
                    current: self.meta.workspace.root.clone(),
                }
            } else {
                if let Err(reason) = self.commit_note(command, note).await {
                    return rejected(reason);
                }
                DirectoryOutcome::Moved
            }
        };
        Ack::Accepted {
            result: serde_json::to_value(outcome).unwrap_or_default(),
        }
    }

    /// `Command::SetWorkspace`（调用方给出说明）：空闲时换工作目录，说明作为 meta 用户消息提交；运行中拒绝。
    pub(crate) async fn set_workspace(
        &mut self,
        command: CommandId,
        root: EnvPath,
        note: Option<String>,
    ) -> Ack {
        if self.busy() {
            return rejected(BUSY);
        }
        if let Err(error) = self.change_workspace(root).await {
            return rejected(error.to_string());
        }
        if let Some(note) = note
            && let Err(reason) = self.commit_note(command, note).await
        {
            return rejected(reason);
        }
        accepted()
    }

    /// 给模型的说明（来源 `rust-cli/cd` 的 meta 用户消息），先提交再进历史、发事件。
    async fn commit_note(&mut self, command: CommandId, note: String) -> Result<(), String> {
        let message = Message::User(UserMessage {
            id: self.deps.ids.message(),
            content: vec![ContentBlock::Text { text: note }],
            origin: UserOrigin::Meta {
                source: NOTE_SOURCE.to_owned(),
            },
        });
        let fact = Fact::MessageAppended {
            message: message.clone(),
        };
        self.commit(Some(command), vec![fact]).await?;
        self.messages.push(message.clone());
        self.log.append(KernelEvent::MessageDone { message });
        Ok(())
    }
}

/// `/cd` 的说明消息的来源标签。
pub(crate) const NOTE_SOURCE: &str = "rust-cli/cd";

/// 回合进行中改目录的拒绝文字。
const BUSY: &str = "A turn is in progress — the working directory can only change while the session is idle. Wait for the turn to finish (or interrupt it), then retry.";

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
