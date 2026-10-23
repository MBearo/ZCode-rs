//! Bash 的超时（docs/specs/rust-cli/cli-args.md §9.3、shell-permissions.md §1.1，修复线 F42）：入口按
//! `BASH_DEFAULT_TIMEOUT_MS` / `BASH_MAX_TIMEOUT_MS` 给出，这里推出前台与后台的缺省与上限，并生成工具文字里
//! 带数字的几句。

/// 缺省值（前台的缺省与上限、后台的缺省与上限的下限，以及 32 位上限）。
const DEFAULT_MS: u64 = 120_000;
const MAX_MS: u64 = 600_000;
const BACKGROUND_DEFAULT_MS: u64 = 600_000;
const BACKGROUND_MAX_MS: u64 = 7_200_000;
const TIMER_LIMIT_MS: u64 = rust_cli_base::js::MAX_TIMER_MS;
/// stream-json 输入时后台命令的缺省期限的下限（headless.md §6.1）。
const STREAMING_BACKGROUND_DEFAULT_MS: u64 = 1_800_000;

/// 前台与后台命令的缺省与最大超时（毫秒）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BashTimeouts {
    pub default_ms: u64,
    pub max_ms: u64,
    pub background_default_ms: u64,
    pub background_max_ms: u64,
}

impl Default for BashTimeouts {
    fn default() -> Self {
        Self::new(None, None)
    }
}

impl BashTimeouts {
    /// `default` / `max` 是两个环境变量读出的正数；没设或无效时是 None。
    /// 前台缺省；前台上限 = max(设的上限或 600000, 前台缺省)；后台缺省（`-p`）= max(600000, 前台
    /// 缺省)；后台上限 = min(max(7200000, 前台上限), 2147483647)。
    pub fn new(default: Option<u64>, max: Option<u64>) -> Self {
        let default_ms = default.unwrap_or(DEFAULT_MS);
        let max_ms = max.unwrap_or(MAX_MS).max(default_ms);
        Self {
            default_ms,
            max_ms,
            background_default_ms: BACKGROUND_DEFAULT_MS.max(default_ms),
            background_max_ms: BACKGROUND_MAX_MS.max(max_ms).min(TIMER_LIMIT_MS),
        }
    }

    /// stream-json 输入（修复线 F39）：后台缺省是 max(1800000, 前台缺省)，
    /// 不是单次 `-p` 的 max(600000, 前台缺省)；其余不变。
    #[must_use]
    pub fn with_streaming_input(self) -> Self {
        Self {
            background_default_ms: STREAMING_BACKGROUND_DEFAULT_MS.max(self.default_ms),
            ..self
        }
    }

    /// 实际使用的超时：没给用缺省，超过上限按上限（取较小者，不报错）。
    pub(crate) fn effective(&self, requested: Option<f64>, background: bool) -> u64 {
        let (default, max) = if background {
            (self.background_default_ms, self.background_max_ms)
        } else {
            (self.default_ms, self.max_ms)
        };
        match requested {
            Some(value) if value.is_finite() && value >= 1.0 => (value as u64).min(max),
            _ => default,
        }
    }

    /// 描述里前台超时的一句。
    pub(crate) fn foreground_line(&self) -> String {
        format!(
            "- `timeout` is in milliseconds: default {}, max {} for a foreground command.",
            self.default_ms, self.max_ms
        )
    }

    /// 描述里后台期限的一句（交互会话去掉）。
    pub(crate) fn deadline_sentence(&self) -> String {
        format!(
            " With it, `timeout` is how long the command may run in the background (default {}, max {}); at that limit it is stopped and you are re-invoked.",
            self.background_default_ms, self.background_max_ms
        )
    }

    /// schema 里 `run_in_background` 说明的后台期限。
    pub(crate) fn deadline_parameter(&self) -> String {
        format!(
            " With it, `timeout` limits how long the command may run in the background before it is stopped (default {} ms, max {} ms).",
            self.background_default_ms, self.background_max_ms
        )
    }

    /// schema 里 `timeout` 的说明。
    pub(crate) fn timeout_parameter(&self) -> String {
        format!(
            "Optional timeout in milliseconds (max {} for a foreground command)",
            self.max_ms
        )
    }
}

#[cfg(test)]
#[path = "bash_timeouts_tests.rs"]
mod tests;
