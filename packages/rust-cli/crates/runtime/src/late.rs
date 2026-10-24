//! 先建好、后接上的压缩器：打破组装时的环（README §11 F11，审查 R1-13 保留的唯一一处晚绑定）。
//!
//! 环：压缩器的摘要请求要用主线程的工具注册表（与主线程共享缓存前缀，compact.md §3.2）；注册表来自 agents；
//! agents 的子循环工厂（`LoopFactory`）又要带上压缩器。组装时先把这个句柄交给子循环，建好压缩器后设置一次，
//! 之后只转发。`LocalKernel::with_extensions` 返回之前一定已经设置，会话与子循环都在那之后才跑。
//!
//! 缺陷修复（R1-13）：原来没接上时 `auto_compact` 静默回 `NotNeeded`（等于悄悄关掉自动压缩），`context_usage`
//! 等有缺省实现的方法没有转发（总是缺省值）。现在每个方法都转发；没接上时返回错误或记 error 日志，不再静默兜底。

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use rust_cli_kernel_api::compaction::{
    AutoCompactOutcome, CompactRequest, CompactResult, CompactTracking, Compactor,
};
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::orchestration::OrchestrationError;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub(crate) struct LateCompactor(OnceLock<Arc<dyn Compactor>>);

impl LateCompactor {
    pub(crate) fn set(&self, compactor: Arc<dyn Compactor>) {
        if self.0.set(compactor).is_err() {
            tracing::error!("the compactor was assembled twice; keeping the first");
        }
    }

    fn inner(&self) -> Result<&Arc<dyn Compactor>, OrchestrationError> {
        self.0.get().ok_or_else(|| OrchestrationError::Failed {
            message: "the compactor is not assembled yet".to_owned(),
        })
    }
}

#[async_trait]
impl Compactor for LateCompactor {
    fn should_auto_compact(&self, history: &[Message], selection: &ModelSelection) -> bool {
        match self.inner() {
            Ok(inner) => inner.should_auto_compact(history, selection),
            Err(error) => {
                tracing::error!(%error, "auto compaction was checked before assembly");
                false
            }
        }
    }

    fn context_usage(&self, history: &[Message], selection: &ModelSelection) -> Option<(u64, u64)> {
        match self.inner() {
            Ok(inner) => inner.context_usage(history, selection),
            Err(error) => {
                tracing::error!(%error, "context usage was asked before assembly");
                None
            }
        }
    }

    fn blocking_limit(
        &self,
        history: &[Message],
        selection: &ModelSelection,
        failure: Option<&str>,
    ) -> Option<String> {
        match self.inner() {
            Ok(inner) => inner.blocking_limit(history, selection, failure),
            Err(error) => {
                tracing::error!(%error, "the blocking limit was checked before assembly");
                None
            }
        }
    }

    async fn auto_compact(
        &self,
        request: CompactRequest,
        tracking: &mut CompactTracking,
        cancel: CancellationToken,
    ) -> Result<AutoCompactOutcome, OrchestrationError> {
        self.inner()?.auto_compact(request, tracking, cancel).await
    }

    async fn compact(
        &self,
        request: CompactRequest,
        cancel: CancellationToken,
    ) -> Result<CompactResult, OrchestrationError> {
        self.inner()?.compact(request, cancel).await
    }
}

#[cfg(test)]
#[path = "late_tests.rs"]
mod tests;
