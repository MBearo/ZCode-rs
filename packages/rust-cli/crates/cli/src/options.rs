//! 入口给内核的可选能力：自动记忆目录（docs/specs/rust-cli/memory.md §2）、压缩配置
//! （docs/specs/rust-cli/compact.md §2）、模型端口的流空闲看门狗（stall-visibility.md §2.2）与任务工具的
//! 开关（agents.md §7）。
//! 环境变量从启动时的快照取。

use std::path::MAIN_SEPARATOR;
use std::sync::Arc;
use std::time::Duration;

use rust_cli_kernel_api::env::{Environment, WorkspaceRef};
use rust_cli_kernel_api::provider::ModelLimits;
use rust_cli_model::{
    BYTE_IDLE_MAX, BYTE_IDLE_MIN, ByteIdle, EVENT_IDLE_FLOOR, STALL_NOTICE, StreamIdle,
};
use rust_cli_paths::StoragePaths;
use rust_cli_runtime::{CompactConfig, CompactThresholds};

fn var<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    rust_cli_base::secret::env_var(env, name)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// 关掉自动压缩并去掉“Run /compact”提示的变量（F55：以前在一个函数里写了两次）。
const DISABLE_COMPACT: &str = "DISABLE_COMPACT";

/// 开关的真值：`1` / `true` / `yes` / `on`（不分大小写）。
fn truthy(env: &[(String, String)], name: &str) -> bool {
    var(env, name).is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// 布尔环境变量的假值（`0` / `false` / `no` / `off`）。
fn falsy(env: &[(String, String)], name: &str) -> bool {
    var(env, name).is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        )
    })
}

/// `IS_SANDBOX` 的名字与两种读法只在这里（修复线 F58，README §4.1）：两处用不同的
/// 真值判断。
const IS_SANDBOX: &str = "IS_SANDBOX";

/// 过载时一直重试：`IS_SANDBOX` 有非空的值（值是字符串，JS 里非空字符串
/// 都是真，`"0"` 也是）。
pub(crate) fn is_sandbox(snapshot: &[(String, String)]) -> bool {
    rust_cli_base::secret::env_var(snapshot, IS_SANDBOX).is_some_and(|value| !value.is_empty())
}

/// root 下允许 bypass 模式（cli-args.md §5.2）：`IS_SANDBOX` 等于 `1`（不等于时拒绝，
/// `flags::refuse_root_bypass`）。
pub(crate) fn is_sandbox_one(snapshot: &[(String, String)]) -> bool {
    rust_cli_base::secret::env_var(snapshot, IS_SANDBOX) == Some("1")
}

/// 任务工具的显式开关：
/// `ZCODE_ENABLE_TASKS` 为假时关闭（优先）；`ZCODE_ENABLE_TODO_TOOLS` 为真时打开（
/// opt-in）；都没有时 None，按模型决定（`rust_cli_agents::task_tools_enabled`，agents.md §7）。
///
/// 环境变量的名字（修复线 F42，config.md §2.5）：本文件读的变量都是 `ZCODE_*`。
///
/// 缺陷修复（R4 F6）：原来入口不读这两个变量，opt-in 恒为假，默认模型下 TaskCreate 报
/// No such tool。
pub(crate) fn task_tools(snapshot: &[(String, String)]) -> Option<bool> {
    if falsy(snapshot, "ZCODE_ENABLE_TASKS") {
        Some(false)
    } else if truthy(snapshot, "ZCODE_ENABLE_TODO_TOOLS") {
        Some(true)
    } else {
        None
    }
}

/// 自动记忆目录：建好后返回给提示词的展示文本（末尾带分隔符）；关闭或建不了时为 None。
pub(crate) async fn memory_dir(
    env: &dyn Environment,
    paths: &StoragePaths,
    workspace: &WorkspaceRef,
    snapshot: &[(String, String)],
) -> Option<String> {
    if truthy(snapshot, "ZCODE_DISABLE_AUTO_MEMORY") {
        return None;
    }
    let project = rust_cli_memory::project_root(env, workspace).await;
    let dir = paths.memory_dir(project.as_str());
    // 建不了目录时不打开记忆段（提示词会说目录已存在）。
    rust_cli_fs::create_dir_all(dir.as_path()).await.ok()?;
    Some(format!("{}{MAIN_SEPARATOR}", dir.as_path().display()))
}

/// `ZCODE_AUTO_COMPACT_WINDOW`：`200k`、`1m`、`100`–`1000`（按千计）或 token 数。
fn window(value: &str) -> Option<u64> {
    let text = value.trim().to_ascii_lowercase();
    let number = |digits: &str| digits.replace(['_', ','], "").parse::<f64>().ok();
    let tokens = if let Some(digits) = text.strip_suffix('m') {
        number(digits)? * 1e6
    } else if let Some(digits) = text.strip_suffix('k') {
        number(digits)? * 1e3
    } else {
        let parsed = number(&text)?;
        if (100.0..=1000.0).contains(&parsed) {
            parsed * 1e3
        } else {
            parsed
        }
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    (tokens.is_finite() && tokens > 0.0).then(|| tokens.round() as u64)
}

/// `limits`：压缩的上限查询（平台层或显式覆盖的内置规则，修复线 F61：两种模式都接，compact.md §2）；
/// `max_output_tokens` 是入口读一次的 `ZCODE_MAX_OUTPUT_TOKENS`（F58）。
pub(crate) fn compact(
    snapshot: &[(String, String)],
    max_output_tokens: Option<u32>,
    limits: Arc<dyn ModelLimits>,
) -> CompactConfig {
    CompactConfig {
        auto: !truthy(snapshot, DISABLE_COMPACT) && !truthy(snapshot, "DISABLE_AUTO_COMPACT"),
        // `DISABLE_COMPACT` 设了时“上下文快满”的提示不接“Run /compact”（compact.md §2.1）。
        compact_command: !truthy(snapshot, DISABLE_COMPACT),
        thresholds: CompactThresholds {
            pct_override: var(snapshot, "ZCODE_AUTOCOMPACT_PCT_OVERRIDE")
                .and_then(|value| value.parse().ok()),
            window_override: var(snapshot, "ZCODE_AUTO_COMPACT_WINDOW").and_then(window),
            // 摘要的输出预留也看输出上限（compact.md §2，F42）。
            max_output_tokens: max_output_tokens.map(u64::from),
        },
        limits: Some(limits),
    }
}

/// 毫秒数，按 JS `parseInt` 取开头的数字；没有数字或为 0 时是没设。
fn millis(env: &[(String, String)], name: &str) -> Option<Duration> {
    let value = var(env, name)?;
    let digits = value
        .find(|c: char| !c.is_ascii_digit())
        .map_or(value, |end| &value[..end]);
    let millis: u64 = digits.parse().ok()?;
    (millis > 0).then(|| Duration::from_millis(millis))
}

/// 开关：`0` / `false` / `no` / `off` 关闭，其余（含没设）开启。
fn enabled(env: &[(String, String)], name: &str) -> bool {
    !var(env, name).is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        )
    })
}

/// 只给测试用的声明（`_ZCODE_ASSUME_*`，model.md §6）是否生效：入口显式传入，单元测试两种都传。
///
/// 工作线 F52b（用户决定 2026-10-22）：只在 debug 构建读，release 构建完全不读、设了也没有作用。以前发布版也读，
/// `_ZCODE_ASSUME_TOOL_SEARCH` 能在任何端点上强制打开 ToolSearch，违背“只在 z.ai / bigmodel 的端点开”。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TestDeclarations {
    Accepted,
    Ignored,
}

impl TestDeclarations {
    /// 本次构建的取值：debug 构建接受（本地对照测试跑 `target/debug/zcode-rs`），release 构建忽略。
    pub(crate) const BUILD: Self = if cfg!(debug_assertions) {
        Self::Accepted
    } else {
        Self::Ignored
    };
}

/// 两个只给测试用的声明的取值（只对显式覆盖的端点生效，平台层的端点照常查方案表）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Assumptions {
    /// `_ZCODE_ASSUME_FIRST_PARTY_BASE_URL`：按 Anthropic 官方
    /// 端点查方案表与能力规则。测试连本地 fixture 时用它发第一方形状。
    pub first_party: bool,
    /// `_ZCODE_ASSUME_TOOL_SEARCH`（工作线 F52）：当作支持延迟加载工具的端点。ToolSearch 只在 z.ai / bigmodel 的端点开
    /// （tools-more.md §3.2），测试的 fixture 在 127.0.0.1，靠它打开 ToolSearch；
    /// 不是给用户的开关。
    pub tool_search: bool,
}

/// 读两个测试声明（取值是 `1` / `true` / `yes` / `on`，不区分大小写）。`Ignored` 时不调用 `var`。
pub(crate) fn assumptions(
    declarations: TestDeclarations,
    var: impl Fn(&str) -> Option<String>,
) -> Assumptions {
    let declared =
        |name: &str| var(name).is_some_and(|value| truthy(&[(name.to_owned(), value)], name));
    match declarations {
        TestDeclarations::Ignored => Assumptions::default(),
        TestDeclarations::Accepted => Assumptions {
            first_party: declared("_ZCODE_ASSUME_FIRST_PARTY_BASE_URL"),
            tool_search: declared("_ZCODE_ASSUME_TOOL_SEARCH"),
        },
    }
}

/// 流空闲看门狗（docs/specs/rust-cli/stall-visibility.md §2.2）：
/// 事件级上限只能调大到不少于 300 s；字节级可单独设，夹在 [10 s, 30 min]，没设时按端点。
pub(crate) fn stream_idle(snapshot: &[(String, String)]) -> StreamIdle {
    let requested = millis(snapshot, "ZCODE_STREAM_IDLE_TIMEOUT_MS");
    let event_limit = requested.map_or(EVENT_IDLE_FLOOR, |limit| limit.max(EVENT_IDLE_FLOOR));
    let bytes = match millis(snapshot, "ZCODE_BYTE_STREAM_IDLE_TIMEOUT_MS") {
        Some(limit) => ByteIdle::Fixed(limit),
        None if requested.is_some() => ByteIdle::Fixed(event_limit),
        None => ByteIdle::Endpoint,
    };
    let bytes = match bytes {
        ByteIdle::Fixed(limit) => ByteIdle::Fixed(limit.clamp(BYTE_IDLE_MIN, BYTE_IDLE_MAX)),
        ByteIdle::Endpoint => ByteIdle::Endpoint,
    };
    StreamIdle {
        events: enabled(snapshot, "ZCODE_ENABLE_STREAM_WATCHDOG").then_some(event_limit),
        bytes: enabled(snapshot, "ZCODE_ENABLE_BYTE_WATCHDOG").then_some(bytes),
        stall_notice: STALL_NOTICE,
    }
}

#[cfg(test)]
#[path = "options_tests.rs"]
mod tests;
