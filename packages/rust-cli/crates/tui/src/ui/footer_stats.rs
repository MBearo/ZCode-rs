//! 请求统计（docs/specs/rust-cli/tui-display.md §2.6，修复线 F27）：`/stats` 卡的缓存命中率与速度历史的来源。值只来自内核：
//! 用量表的查询结果，由 runner 按序号写入 [`FooterStats`]。状态栏不画命中率（用户决定 2026-11-01）。
use rust_cli_kernel_api::metering::{RequestStats, SpeedHistory, Speeds};

/// runner 写入的最后一份请求统计。回执带请求序号：切换会话与 `/clear` 时 [`Self::clear`] 让在途的回执作废，不用超时。
/// 每份收下的统计也记进速度的历史（kernel-api `metering::SpeedHistory`，与 cli 的 Puffin host 同一份规则）：`/stats` 卡的
/// 速度一行画每个回合的数（Puffin `usage.speed.turns`）。
#[derive(Debug, Default)]
pub struct FooterStats {
    seq: u64,
    /// 最新的查询发出时数到的回合结束个数（给速度的历史收尾）。
    ended: u64,
    stats: RequestStats,
    history: SpeedHistory,
    speeds: Speeds,
}

impl FooterStats {
    /// 发一次新查询：序号加一并返回。`ended` 是这时看的会话已经结束的回合数。
    pub fn begin(&mut self, ended: u64) -> u64 {
        self.seq += 1;
        self.ended = ended;
        self.seq
    }

    /// 查询的回执：序号是最新的才收下（返回是否收下）；查询失败（存储不支持）时当作没有值。
    pub fn arrived(&mut self, seq: u64, stats: Option<RequestStats>) -> bool {
        if seq != self.seq {
            return false;
        }
        if let Some(stats) = &stats {
            self.speeds = self.history.record(stats, self.ended);
        }
        self.stats = stats.unwrap_or_default();
        true
    }

    /// 离开会话：清空，在途的回执作废。
    pub fn clear(&mut self) {
        self.seq += 1;
        self.stats = RequestStats::default();
        self.history = SpeedHistory::default();
        self.speeds = Speeds::default();
    }

    pub fn stats(&self) -> &RequestStats {
        &self.stats
    }

    /// 量到的速度：最近一次与每个回合的（Puffin `usage.speed`）。
    pub fn speeds(&self) -> &Speeds {
        &self.speeds
    }
}
