//! 定时任务的触发行（docs/specs/rust-cli/tools-more.md §18.3）与它们的折叠。
//!
//! 行只在视图里：内核不落库，重新取快照后没有（D43）。折叠时收起上一个 loop 触发行到末尾的行（隐藏
//! 上一个 tick）。
use rust_cli_kernel_api::cron::CronFireKind;

use super::{Body, Fire, Transcript};

impl Transcript {
    pub(super) fn fire(&mut self, fire: Fire, now_ms: u64) {
        if fire.fold.is_some()
            && let Some(at) = self.rows.iter().rposition(
                |row| matches!(&row.body, Body::Fire(previous) if previous.kind == CronFireKind::Loop),
            )
        {
            self.rows.truncate(at);
        }
        self.push(Body::Fire(Box::new(fire)), now_ms);
    }
}

#[cfg(test)]
#[path = "fire_tests.rs"]
mod tests;
