//! 会话的输出速度的历史（docs/puffin-tui.md §5 `speed`）：按每次取到的请求统计
//! 算出 `last` 与 `turns`。旧界面给 `/stats` 卡的速度一行（tui-kit.md
//! §4.2「`/stats`」）。以前这段只在 cli 的 Puffin host 里。
//!
//! - 请求数比上次多：最近一次请求是新结束的调用。它输出至少 [`MIN_OUTPUT`] 个 token、量得到速度时，`last` 换成它的
//!   速度，它的输出与生成时长计入当前回合。两次查询之间结束了两次以上时只认最后一次（请求统计只有最近一次）。
//! - 回合的收尾：查询带着发出时数到的 `TurnFinished` 个数。存储在回合结束之前写下了它的全部请求，所以这个数
//!   比已收尾的多时，这次查询已看到回合的全部调用：当前回合有量到的调用就记下一个数（输出之和 ÷ 生成时长之和）。
//! - 第一次查询是冷加载：只有最近一次请求，`last` 与 `turns` 都只有它一个数，不计入哪个回合。
use super::{RequestStats, output_tps};

/// 量到的速度（tok/s）：`last` 是最近一次输出至少 [`MIN_OUTPUT`] 个 token 的调用；`turns` 每个有这种调用的回合一个数，
/// 最早的在前。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Speeds {
    pub last: Option<f64>,
    pub turns: Vec<f64>,
}

/// 速度只算输出至少这么多 token 的调用（docs/puffin-tui.md §4.2「多少」的「速度」）。
pub const MIN_OUTPUT: u64 = 50;

/// 记着的历史（`record` 之间的状态）。
#[derive(Debug, Default, Clone)]
pub struct SpeedHistory {
    primed: bool,
    requests: u64,
    last: Option<f64>,
    turns: Vec<f64>,
    /// 当前回合里量到的调用的输出之和与生成时长之和。
    output: u64,
    generation_ms: u64,
    /// 已收尾的回合数（与查询带的 `TurnFinished` 个数比）。
    closed: u64,
}

impl SpeedHistory {
    /// 听说一次请求统计；`ended` 是这次查询发出时 host 数到的 `TurnFinished` 个数。
    pub fn record(&mut self, stats: &RequestStats, ended: u64) -> Speeds {
        let call = stats.last.and_then(|last| {
            let output = last.output_tokens?;
            let generation = last.generation_ms()?;
            let tps = output_tps(Some(output), Some(generation))?;
            (output >= MIN_OUTPUT).then_some((output, generation, tps))
        });
        if !self.primed {
            self.primed = true;
            if let Some((_, _, tps)) = call {
                self.last = Some(tps);
                self.turns.push(tps);
            }
        } else if stats.requests > self.requests
            && let Some((output, generation, tps)) = call
        {
            self.last = Some(tps);
            self.output += output;
            self.generation_ms += generation;
        }
        self.requests = stats.requests;
        if ended > self.closed {
            self.closed = ended;
            if let Some(tps) = output_tps(Some(self.output), Some(self.generation_ms)) {
                self.turns.push(tps);
            }
            self.output = 0;
            self.generation_ms = 0;
        }
        Speeds {
            last: self.last,
            turns: self.turns.clone(),
        }
    }
}

#[cfg(test)]
#[path = "metering_speeds_tests.rs"]
mod tests;
