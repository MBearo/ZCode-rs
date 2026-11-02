//! ext 测试共用的时钟：kernel-api 的 [`TokioClock`]，跟着 tokio 时间走（暂停的测试时间里截止时刻也准）。

use std::sync::Arc;

use rust_cli_kernel_api::testing::TokioClock;

/// 从 1_000_000 毫秒开始的 tokio 时钟。
pub(crate) fn clock() -> Arc<TokioClock> {
    Arc::new(TokioClock::new(1_000_000))
}
