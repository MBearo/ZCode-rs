//! 测试用的时钟（名字以 `_tests.rs` 结尾：测试文件可以用 `Instant`，README §4）。

use std::sync::Arc;

/// 跟着 tokio 时间走的时钟：暂停的测试时间里截止时刻也准。
pub(crate) struct TokioClock {
    start: tokio::time::Instant,
}

impl TokioClock {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            start: tokio::time::Instant::now(),
        })
    }
}

impl rust_cli_kernel_api::clock::Clock for TokioClock {
    fn now_ms(&self) -> u64 {
        1_000_000 + u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn time_zone(&self) -> String {
        "UTC".to_owned()
    }
}
