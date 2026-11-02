//! 立即发送的键在输入框为空时的替身（修复线 F105，docs/specs/rust-cli/tui.md §5.1.1）：后台命令端口记下“把前台命令转到
//! 后台”的请求，机器的其余端口返回 `Unsupported`。测试在 `queue.rs`。
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rust_cli_kernel_api::env::{BackgroundReason, EnvError};
use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
use rust_cli_kernel_api::ids::{SessionId, ShellTaskId};
use rust_cli_kernel_api::machine::Background;
use rust_cli_kernel_api::testing::{registry, unsupported_machine};

/// 记下每次 `background_foreground` 的原因（没有前台命令，转了 0 个）。
#[derive(Default)]
pub struct MovesEnv {
    moved: Mutex<Vec<BackgroundReason>>,
}

impl MovesEnv {
    /// 只有主 env 的注册表：机器只换上后台命令端口（其余端口返回 `Unsupported`）。
    pub fn registry(self: Arc<Self>) -> Arc<dyn EnvironmentRegistry> {
        registry(unsupported_machine().with_background(self))
    }

    pub fn moved(&self) -> Vec<BackgroundReason> {
        self.moved.lock().unwrap().clone()
    }

    /// 等到记下 `count` 次请求（最多 10 秒，同界面的等待）。
    pub async fn wait_moved(&self, count: usize) {
        let wait = async {
            while self.moved.lock().unwrap().len() < count {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        if tokio::time::timeout(Duration::from_secs(10), wait)
            .await
            .is_err()
        {
            panic!(
                "timed out waiting for {count} background requests: {:?}",
                self.moved()
            );
        }
    }
}

#[async_trait]
impl Background for MovesEnv {
    async fn background_foreground(
        &self,
        _session: &SessionId,
        reason: BackgroundReason,
    ) -> Result<Vec<ShellTaskId>, EnvError> {
        self.moved.lock().unwrap().push(reason);
        Ok(Vec::new())
    }
}
