//! TUI 的输入历史（docs/specs/rust-cli/tui.md，与 Host TUI 共用 `<dataBaseDir>/.zcode/v2/tui/history.jsonl`）：
//! 每行 `{"workspace", "text", "at"}`。启动时读本工作区最近 [`LOAD`] 条；发送时交给唯一的写者，按发送顺序
//! 追加（同一个文件只有一个写者，不需要加锁）。

use rust_cli_paths::{StoragePath, StoragePaths};
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// 启动时读入的条数（与 Host TUI 相同）。
pub const LOAD: usize = 200;

pub struct History {
    path: StoragePath,
}

/// 写者的入口：TUI 发送时调用 [`Recorder::record`]，不等待写完。
#[derive(Debug, Clone)]
pub struct Recorder {
    sender: mpsc::UnboundedSender<(String, u64)>,
}

impl Recorder {
    /// `at_ms`：发送时刻（毫秒）。写者已经结束时丢弃（进程正在退出）。
    pub fn record(&self, text: &str, at_ms: u64) {
        let _ = self.sender.send((text.to_owned(), at_ms));
    }
}

impl History {
    pub fn new(paths: &StoragePaths) -> Self {
        Self {
            path: paths.tui_history(),
        }
    }

    /// 这个工作区最近的历史，从旧到新；文件不存在、读不了或损坏的行都跳过。
    pub async fn load(&self, workspace: &str) -> Vec<String> {
        let Ok(Some(bytes)) = rust_cli_fs::read_optional(self.path.as_path()).await else {
            return Vec::new();
        };
        let text = String::from_utf8_lossy(&bytes);
        let mut entries: Vec<String> = text
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|entry| entry["workspace"] == workspace)
            .filter_map(|entry| entry["text"].as_str().map(str::to_owned))
            .collect();
        let skip = entries.len().saturating_sub(LOAD);
        entries.drain(..skip);
        entries
    }

    /// 起写者：返回给 TUI 的入口与要放进任务作用域的写者循环。入口全部释放后，写完剩下的就结束。
    pub fn writer(
        self,
        workspace: String,
    ) -> (Recorder, impl Future<Output = ()> + Send + 'static) {
        let (sender, mut entries) = mpsc::unbounded_channel::<(String, u64)>();
        let task = async move {
            while let Some((text, at)) = entries.recv().await {
                let mut line = json!({"workspace": workspace, "text": text, "at": at}).to_string();
                line.push('\n');
                if let Err(error) = rust_cli_fs::append(self.path.as_path(), line.as_bytes()).await
                {
                    tracing::warn!(%error, "the input history could not be written");
                }
            }
        };
        (Recorder { sender }, task)
    }
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
