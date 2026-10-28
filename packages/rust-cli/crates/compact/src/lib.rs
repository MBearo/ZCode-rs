//! 上下文压缩（docs/specs/rust-cli/compact.md）。
//!
//! 一个压缩核心（分组、保留尾段、PTL 阶梯、摘要消息与边界、压缩后重读
//! 文件）供 `/compact`、阈值触发的自动压缩与上下文超限后的反应式压缩共用；自动压缩的窗口、阈值与
//! 熔断；micro 压缩的算法（默认不触发，不接线）。
//!
//! 摘要请求经 [`QueryRunner`] 跑 `QuerySource::Compact` 的子循环；它的执行器是 [`DenyTools`]、
//! 压缩器是 [`Disabled`]（由 runtime 组装）。本 crate 不做 IO。

mod blocking;
mod engine;
mod fallback;
mod groups;
mod hooks;
pub mod micro;
mod request;
mod restore;
mod summary;
mod tokens;
pub mod window;

#[cfg(test)]
mod test_support;

pub use request::DenyTools;
pub use tokens::{IMAGE_TOKENS, measured};
pub use window::Thresholds;

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::compaction::{ContextNotice, PostCompactContext};
use rust_cli_kernel_api::history;
use rust_cli_kernel_api::ids::Generation;
use rust_cli_kernel_api::local_services::LocalServices;
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_kernel_api::message::{CompactTrigger, Message};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::orchestration::{
    AutoCompactOutcome, CompactRequest, CompactResult, CompactTracking, Compactor, HookRunner,
    OrchestrationError, QueryRunner, ToolRegistry,
};
use rust_cli_kernel_api::provider::ModelLimits;
use rust_cli_kernel_api::store::ArtifactStore;
use rust_cli_kernel_api::tool::ReadFileStates;
use tokio_util::sync::CancellationToken;

use crate::engine::{Engine, Failure};
use crate::window::ModelFacts;

/// 连续失败几次后本次运行不再自动压缩。
const MAX_CONSECUTIVE_FAILURES: u32 = 3;
/// 上一次压缩后不到这么多工具轮又要压缩，算一次 rapid refill。
const RAPID_REFILL_TURNS: u32 = 3;
/// rapid refill 熔断的次数。
const RAPID_REFILL_LIMIT: u32 = 3;
/// rapid refill 熔断时的错误原文。
const THRASHING: &str = "Autocompact is thrashing: the context refilled to the limit within 3 turns of the previous compact, 3 times in a row. A file being read or a tool output is likely too large for the context window. Try reading in smaller chunks, or use /clear to start fresh.";

/// 压缩用到的能力（runtime 组装）。
#[derive(Clone)]
pub struct CompactPorts {
    /// 摘要请求的子循环（执行器 [`DenyTools`]、压缩器 [`Disabled`]）。
    pub runner: Arc<dyn QueryRunner>,
    /// 与主循环相同的工具注册表：重读文件用它的 Read 工具。
    pub tools: Arc<dyn ToolRegistry>,
    /// 与文件工具同一份 readFileState：压缩收尾时清空，用清空前的路径选要重读的文件（spec §3.4）。
    pub files: Arc<dyn ReadFileStates>,
    /// 压缩附件里的清单增量（context 实现，spec §3.4）。
    pub context: Arc<dyn PostCompactContext>,
    pub hooks: Arc<dyn HookRunner>,
    pub ids: Arc<dyn Ids>,
    /// 压缩后重读文件的那台机器（经 Read 工具；R0 只有本机，remote-exec.md §15.8.1）。
    pub machine: MachineEnv,
    /// Read 工具的上下文要带本机服务（重读用不到）。
    pub local: LocalServices,
    pub artifacts: Arc<dyn ArtifactStore>,
    pub generation: Generation,
}

/// 入口从设置与环境变量构造（自动压缩的设置、`DISABLE_COMPACT`；`ZCODE_AUTOCOMPACT_PCT_OVERRIDE`、
/// `ZCODE_AUTO_COMPACT_WINDOW`、`ZCODE_MAX_OUTPUT_TOKENS`，修复线 F42）。
#[derive(Clone)]
pub struct CompactConfig {
    /// 自动压缩（含反应式）打开。手动 `/compact` 不受它影响。
    pub auto: bool,
    /// `/compact` 能用（`DISABLE_COMPACT` 没设）。只决定“上下文快满”提示的后半句（compact.md §2.1）。
    pub compact_command: bool,
    pub thresholds: Thresholds,
    /// 模型上限查询（compact.md §2，修复线 F61）：窗口与输出上限的唯一来源，入口在两种模式下都接上（平台层、显式
    /// 覆盖的内置规则）。没有时（测试）按卡片与默认窗口。
    pub limits: Option<Arc<dyn ModelLimits>>,
}

impl Default for CompactConfig {
    fn default() -> Self {
        Self {
            auto: true,
            compact_command: true,
            thresholds: Thresholds::default(),
            limits: None,
        }
    }
}

impl CompactConfig {
    /// 这个选择的窗口与输出上限（模型上限查询 → 卡片 → 默认窗口）。自动压缩的判定、footer 的 ctx、“上下文快满”
    /// 的提示与 `/context` 都只经这里（compact.md §2）。
    ///
    /// 修 bug（F61）：以前有“有目录 / 没目录”两条路——只有异步的 `auto_compact` 读目录，三个同步方法传“没有目录”，
    /// 显式覆盖连目录也没有，没有卡片的模型（例如配置 1M 的 glm-5.3）按 200,000 压缩、显示。
    pub fn facts(&self, selection: &ModelSelection) -> ModelFacts {
        let limits = self
            .limits
            .as_ref()
            .and_then(|limits| limits.limits(selection));
        ModelFacts::resolve(selection, limits)
    }
}

pub struct Compaction {
    ports: CompactPorts,
    config: CompactConfig,
}

fn failed(failure: &Failure) -> OrchestrationError {
    OrchestrationError::Failed {
        message: failure.to_string(),
    }
}

impl Compaction {
    pub fn new(ports: CompactPorts, config: CompactConfig) -> Self {
        Self { ports, config }
    }

    fn tokens(history: &[Message]) -> u64 {
        let active = history::active(history);
        let refs: Vec<&Message> = active.iter().map(AsRef::as_ref).collect();
        rust_cli_kernel_api::compaction::context_tokens(&refs)
    }

    async fn run(
        &self,
        request: &CompactRequest,
        fallback: bool,
        cancel: &CancellationToken,
    ) -> Result<CompactResult, Failure> {
        Engine { ports: &self.ports }
            .run(request, fallback, cancel)
            .await
    }
}

#[async_trait]
impl Compactor for Compaction {
    fn should_auto_compact(&self, history: &[Message], selection: &ModelSelection) -> bool {
        self.config.auto
            && window::should_compact(
                Self::tokens(history),
                &self.config.facts(selection),
                &self.config.thresholds,
            )
    }

    /// footer 的 ctx（tui-display.md §2.6，修复线 F27）：`(token, 有效窗口)`，与 [`Self::context_notice`] 同一套 token、
    /// 窗口（[`CompactConfig::facts`]）与覆盖。F14 起这里没有实现（提示改成 `context_notice`），F27 起会话 actor 又发这一对值。
    fn context_usage(&self, history: &[Message], selection: &ModelSelection) -> Option<(u64, u64)> {
        let facts = self.config.facts(selection);
        Some((
            Self::tokens(history),
            window::effective(&facts, &self.config.thresholds),
        ))
    }

    /// 输入框下方“上下文快满”的提示（compact.md §2.1）：与 [`Self::should_auto_compact`] 同一套 token、窗口
    /// （[`CompactConfig::facts`]）、来源与开关。
    fn context_notice(
        &self,
        history: &[Message],
        selection: &ModelSelection,
    ) -> Option<ContextNotice> {
        let facts = self.config.facts(selection);
        Some(window::notice(
            Self::tokens(history),
            &facts,
            &self.config.thresholds,
            self.config.auto,
            self.config.compact_command,
        ))
    }

    async fn auto_compact(
        &self,
        mut request: CompactRequest,
        tracking: &mut CompactTracking,
        cancel: CancellationToken,
    ) -> Result<AutoCompactOutcome, OrchestrationError> {
        if !self.config.auto || tracking.consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
            return Ok(AutoCompactOutcome::NotNeeded);
        }
        let facts = self.config.facts(&request.selection);
        let tokens = Self::tokens(&request.messages);
        if !window::should_compact(tokens, &facts, &self.config.thresholds) {
            return Ok(AutoCompactOutcome::NotNeeded);
        }
        let rapid = if tracking.compacted && tracking.turn_counter < RAPID_REFILL_TURNS {
            tracking.consecutive_rapid_refills + 1
        } else {
            0
        };
        if rapid >= RAPID_REFILL_LIMIT {
            return Ok(AutoCompactOutcome::Thrashing {
                message: THRASHING.to_owned(),
            });
        }
        request.trigger = CompactTrigger::Auto;
        // 阈值触发只在达到阻塞线时许“全部摘要”（compact.md §3.1）。
        let blocked =
            window::level(tokens, &facts, &self.config.thresholds) == window::Level::Blocked;
        match self.run(&request, blocked, &cancel).await {
            Ok(result) => {
                *tracking = CompactTracking {
                    compacted: true,
                    turn_counter: 0,
                    consecutive_failures: 0,
                    consecutive_rapid_refills: rapid,
                };
                Ok(AutoCompactOutcome::Compacted {
                    result: Box::new(result),
                })
            }
            // 被 PreCompact hook 拦下不算失败，照常发请求。
            Err(Failure::Blocked(_)) => Ok(AutoCompactOutcome::NotNeeded),
            Err(failure) => {
                tracking.consecutive_failures += 1;
                Ok(AutoCompactOutcome::Failed {
                    message: failure.to_string(),
                })
            }
        }
    }

    /// 发请求前的阻塞线（compact.md §2.2）：与自动压缩同一套窗口、token 与开关。
    fn blocking_limit(
        &self,
        history: &[Message],
        selection: &ModelSelection,
        failure: Option<&str>,
    ) -> Option<String> {
        let facts = self.config.facts(selection);
        let thresholds = &self.config.thresholds;
        // 不拦的情形先判断，不为它数 token。
        if self.config.auto
            && window::window(&facts, thresholds).source == window::WindowSource::Auto
        {
            return None;
        }
        window::blocks(Self::tokens(history), &facts, thresholds, self.config.auto)
            .then(|| blocking::text(failure))
    }

    async fn compact(
        &self,
        request: CompactRequest,
        cancel: CancellationToken,
    ) -> Result<CompactResult, OrchestrationError> {
        if request.trigger != CompactTrigger::Manual && !self.config.auto {
            return Err(OrchestrationError::Unsupported {
                what: "automatic compaction is disabled".to_owned(),
            });
        }
        // 手动与反应式压缩许“全部摘要”（没有媒体超限错误时）。
        self.run(&request, true, &cancel)
            .await
            .map_err(|f| failed(&f))
    }
}

/// 不压缩：摘要子循环自己不能再压缩（`compact` 来源不做自动与反应式压缩）。
pub struct Disabled;

#[async_trait]
impl Compactor for Disabled {
    fn should_auto_compact(&self, _history: &[Message], _selection: &ModelSelection) -> bool {
        false
    }

    async fn compact(
        &self,
        _request: CompactRequest,
        _cancel: CancellationToken,
    ) -> Result<CompactResult, OrchestrationError> {
        Err(OrchestrationError::Unsupported {
            what: "compaction inside a compaction request".to_owned(),
        })
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "restore_state_tests.rs"]
mod restore_state_tests;

#[cfg(test)]
#[path = "post_compact_tests.rs"]
mod post_compact_tests;

#[cfg(test)]
#[path = "limits_tests.rs"]
mod limits_tests;
