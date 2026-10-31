//! 入口给内核的可选能力：自动记忆目录（docs/specs/rust-cli/memory.md §2）、压缩配置
//! （docs/specs/rust-cli/compact.md §2）、模型端口的流空闲看门狗（stall-visibility.md §2.2）与任务工具的
//! 开关（agents.md §7）。
//! 环境变量从启动时的快照取。

use std::ops::RangeInclusive;
use std::path::MAIN_SEPARATOR;
use std::sync::Arc;
use std::time::Duration;

use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::machine::Files;
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
const DISABLE_COMPACT: &str = rust_cli_base::names::env::DISABLE_COMPACT;

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
const IS_SANDBOX: &str = rust_cli_base::names::env::IS_SANDBOX;

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
    if falsy(snapshot, rust_cli_base::names::env::ZCODE_ENABLE_TASKS) {
        Some(false)
    } else if truthy(snapshot, rust_cli_base::names::env::ZCODE_ENABLE_TODO_TOOLS) {
        Some(true)
    } else {
        None
    }
}

/// 自动记忆目录：建好后返回给提示词的展示文本（末尾带分隔符）；关闭或建不了时为 None。
pub(crate) async fn memory_dir(
    files: &dyn Files,
    paths: &StoragePaths,
    workspace: &WorkspaceRef,
    snapshot: &[(String, String)],
) -> Option<String> {
    if truthy(
        snapshot,
        rust_cli_base::names::env::ZCODE_DISABLE_AUTO_MEMORY,
    ) {
        return None;
    }
    let project = rust_cli_memory::project_root(files, workspace).await;
    let dir = paths.memory_dir(project.as_str());
    // 建不了目录时不打开记忆段（提示词会说目录已存在）。
    rust_cli_fs::create_dir_all(dir.as_path()).await.ok()?;
    Some(format!("{}{MAIN_SEPARATOR}", dir.as_path().display()))
}

/// `m` 后缀的倍数（百万 token）。
const MILLION_TOKENS: f64 = 1e6;
/// `k` 后缀与按千计的写法的倍数（千 token）。
const THOUSAND_TOKENS: f64 = 1e3;
/// 不带后缀时按千计的取值范围（`200` 是 200k）。
const BARE_THOUSANDS: RangeInclusive<f64> = 100.0..=1000.0;

/// `ZCODE_AUTO_COMPACT_WINDOW`：`200k`、`1m`、`100`–`1000`（按千计）或 token 数。
fn window(value: &str) -> Option<u64> {
    let text = value.trim().to_ascii_lowercase();
    let number = |digits: &str| digits.replace(['_', ','], "").parse::<f64>().ok();
    let tokens = if let Some(digits) = text.strip_suffix('m') {
        number(digits)? * MILLION_TOKENS
    } else if let Some(digits) = text.strip_suffix('k') {
        number(digits)? * THOUSAND_TOKENS
    } else {
        let parsed = number(&text)?;
        if BARE_THOUSANDS.contains(&parsed) {
            parsed * THOUSAND_TOKENS
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
        auto: !truthy(snapshot, DISABLE_COMPACT)
            && !truthy(snapshot, rust_cli_base::names::env::DISABLE_AUTO_COMPACT),
        // `DISABLE_COMPACT` 设了时“上下文快满”的提示不接“Run /compact”（compact.md §2.1）。
        compact_command: !truthy(snapshot, DISABLE_COMPACT),
        thresholds: CompactThresholds {
            pct_override: var(
                snapshot,
                rust_cli_base::names::env::ZCODE_AUTOCOMPACT_PCT_OVERRIDE,
            )
            .and_then(|value| value.parse().ok()),
            window_override: var(
                snapshot,
                rust_cli_base::names::env::ZCODE_AUTO_COMPACT_WINDOW,
            )
            .and_then(window),
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Assumptions {
    /// `_ZCODE_ASSUME_FIRST_PARTY_BASE_URL`：按 Anthropic 官方
    /// 端点查方案表与能力规则。测试连本地 fixture 时用它发第一方形状。
    pub first_party: bool,
    /// `_ZCODE_ASSUME_TOOL_SEARCH`（工作线 F52）：当作支持延迟加载工具（`tool_reference`）的端点，用原生 ToolSearch。
    /// 工作线 F89 起产品路径一律本地（tools-more.md §3.2.2），这是选到原生的唯一途径，原生的实现靠它保留测试；不声明时
    /// 是本地 ToolSearch（F84 时原生只给 z.ai / bigmodel 的端点；F84 以前不声明是不开）。不是给用户的开关。
    pub tool_search: bool,
    /// `_ZCODE_ASSUME_MONITOR_OFF`（工作线 F83）：关掉 Monitor 与它带动的文字，等于对齐基准的开关关着
    /// （tools-more.md §19.1）。不是给用户的开关。
    pub monitor_off: bool,
    /// `_ZCODE_ASSUME_LOOPBACK_EXECUTOR`（远端执行 R1，remote-exec.md §18.6）：本机的执行端口经 loopback 走子进程
    /// `zcode executor --stdio`，验证远端实现与本机结果相同。不是给用户的开关。
    pub loopback: bool,
    /// `_ZCODE_ASSUME_LOOPBACK_MACHINES`（远端执行 R3，remote-exec.md §20.10）：`machines.json` 里连着的机器经 loopback
    /// 连接（每台一个执行器子进程，工作目录是那台机器的工作目录），`-p` 也连。不是给用户的开关。
    pub loopback_machines: bool,
    /// `_ZCODE_ASSUME_RULE_MODEL=<模型 ID>`（工作线 F121，model.md §11.1）：显式覆盖的能力与上限按这个模型 ID 查内置规则，
    /// 请求里的模型仍是选择的模型。轨迹比对的本工程一侧用它让 GLM 拿到对齐基准里 opus 的能力。不是给用户的开关。
    pub rule_model: Option<String>,
    /// `_ZCODE_ASSUME_SSH_MACHINES`（远端执行 R3，remote-exec.md §20.10）：`-p` 也经 SSH 连 `machines.json` 里连着的机器
    /// （同步连完再开始），`_ZCODE_ASSUME_SSH_CONFIG` / `_ZCODE_ASSUME_SSH_ARTIFACT` 用于这些机器。不是给用户的开关。
    pub ssh_machines: bool,
}

/// 机器清单里的机器经 SSH 时，只给测试的 ssh 配置文件与上传用的产物（R3，remote-exec.md §20.10；只在 debug 构建读）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct MachineSsh {
    /// `_ZCODE_ASSUME_SSH_CONFIG`。
    pub config: Option<String>,
    /// `_ZCODE_ASSUME_SSH_ARTIFACT`。
    pub artifact: Option<String>,
    /// `_ZCODE_ASSUME_NPM_REGISTRY`（F100，remote-exec.md §26.7）：执行器的 npm 来源换成这个假注册表的根（debug 构建的版本
    /// npm 上没有，不给就不下载）。不是给用户的开关。
    pub npm_registry: Option<String>,
}

/// 读 [`MachineSsh`]；`Ignored` 时什么都不读。
pub(crate) fn machine_ssh(
    declarations: TestDeclarations,
    var: impl Fn(&str) -> Option<String>,
) -> MachineSsh {
    if declarations == TestDeclarations::Ignored {
        return MachineSsh::default();
    }
    let value = |name: &str| var(name).filter(|value| !value.trim().is_empty());
    MachineSsh {
        config: value(rust_cli_base::names::env::_ZCODE_ASSUME_SSH_CONFIG),
        artifact: value(rust_cli_base::names::env::_ZCODE_ASSUME_SSH_ARTIFACT),
        npm_registry: value(rust_cli_base::names::env::_ZCODE_ASSUME_NPM_REGISTRY),
    }
}

/// `_ZCODE_ASSUME_BASE_URL`（修复线 F113，docs/specs/rust-cli/provider-errors.md D13）：查请求方案与厂商的错误码表时
/// 把端点当成这个地址，请求仍发到真实地址（本地夹具走到智谱的方案与码表）。不是给用户的开关；`Ignored` 时不读。
pub(crate) fn assumed_base_url(
    declarations: TestDeclarations,
    var: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    if declarations == TestDeclarations::Ignored {
        return None;
    }
    var(rust_cli_base::names::env::_ZCODE_ASSUME_BASE_URL).filter(|value| !value.trim().is_empty())
}

/// 只给测试的 SSH 入口（远端执行 R2，remote-exec.md §19.11）：本机的执行端口换成经 SSH 连到 `destination` 的远端环境。
/// 只在 debug 构建读（同 `_ZCODE_ASSUME_*`）；不是给用户的开关（机器清单与 `/machines` 在 R3 / R4）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SshAssumption {
    /// `_ZCODE_ASSUME_SSH_EXECUTOR`：SSH 目标。
    pub destination: String,
    /// `_ZCODE_ASSUME_SSH_CONFIG`：给 ssh 的 `-F <文件>`。
    pub config: Option<String>,
    /// `_ZCODE_ASSUME_SSH_ARTIFACT`：上传用的 Linux 产物（架构从 ELF 头读）。
    pub artifact: Option<String>,
}

/// 读 SSH 测试入口；没设目标或 `Ignored` 时 None（`Ignored` 时不调用 `var`）。
pub(crate) fn ssh_assumption(
    declarations: TestDeclarations,
    var: impl Fn(&str) -> Option<String>,
) -> Option<SshAssumption> {
    if declarations == TestDeclarations::Ignored {
        return None;
    }
    let value = |name: &str| var(name).filter(|value| !value.trim().is_empty());
    Some(SshAssumption {
        destination: value(rust_cli_base::names::env::_ZCODE_ASSUME_SSH_EXECUTOR)?,
        config: value(rust_cli_base::names::env::_ZCODE_ASSUME_SSH_CONFIG),
        artifact: value(rust_cli_base::names::env::_ZCODE_ASSUME_SSH_ARTIFACT),
    })
}

/// Monitor 开不开（工作线 F83，用户决定 2026-10-24 产品缺省开）：同 Bash 的条件——Windows 上要找到 Git Bash；只给测试用的
/// 声明关掉。
pub(crate) fn monitor(assume: &Assumptions, bash_found: bool) -> bool {
    !assume.monitor_off && (!cfg!(windows) || bash_found)
}

/// 读测试声明（取值是 `1` / `true` / `yes` / `on`，不区分大小写）。`Ignored` 时不调用 `var`。
pub(crate) fn assumptions(
    declarations: TestDeclarations,
    var: impl Fn(&str) -> Option<String>,
) -> Assumptions {
    let declared =
        |name: &str| var(name).is_some_and(|value| truthy(&[(name.to_owned(), value)], name));
    match declarations {
        TestDeclarations::Ignored => Assumptions::default(),
        TestDeclarations::Accepted => Assumptions {
            first_party: declared(rust_cli_base::names::env::_ZCODE_ASSUME_FIRST_PARTY_BASE_URL),
            tool_search: declared(rust_cli_base::names::env::_ZCODE_ASSUME_TOOL_SEARCH),
            monitor_off: declared(rust_cli_base::names::env::_ZCODE_ASSUME_MONITOR_OFF),
            loopback: declared(rust_cli_base::names::env::_ZCODE_ASSUME_LOOPBACK_EXECUTOR),
            loopback_machines: declared(rust_cli_base::names::env::_ZCODE_ASSUME_LOOPBACK_MACHINES),
            ssh_machines: declared(rust_cli_base::names::env::_ZCODE_ASSUME_SSH_MACHINES),
            rule_model: var(rust_cli_base::names::env::_ZCODE_ASSUME_RULE_MODEL)
                .map(|model| model.trim().to_owned())
                .filter(|model| !model.is_empty()),
        },
    }
}

/// 流空闲看门狗（docs/specs/rust-cli/stall-visibility.md §2.2）：
/// 事件级上限只能调大到不少于 300 s；字节级可单独设，夹在 [10 s, 30 min]，没设时按端点。
pub(crate) fn stream_idle(snapshot: &[(String, String)]) -> StreamIdle {
    let requested = millis(
        snapshot,
        rust_cli_base::names::env::ZCODE_STREAM_IDLE_TIMEOUT_MS,
    );
    let event_limit = requested.map_or(EVENT_IDLE_FLOOR, |limit| limit.max(EVENT_IDLE_FLOOR));
    let bytes = match millis(
        snapshot,
        rust_cli_base::names::env::ZCODE_BYTE_STREAM_IDLE_TIMEOUT_MS,
    ) {
        Some(limit) => ByteIdle::Fixed(limit),
        None if requested.is_some() => ByteIdle::Fixed(event_limit),
        None => ByteIdle::Endpoint,
    };
    let bytes = match bytes {
        ByteIdle::Fixed(limit) => ByteIdle::Fixed(limit.clamp(BYTE_IDLE_MIN, BYTE_IDLE_MAX)),
        ByteIdle::Endpoint => ByteIdle::Endpoint,
    };
    StreamIdle {
        events: enabled(
            snapshot,
            rust_cli_base::names::env::ZCODE_ENABLE_STREAM_WATCHDOG,
        )
        .then_some(event_limit),
        bytes: enabled(
            snapshot,
            rust_cli_base::names::env::ZCODE_ENABLE_BYTE_WATCHDOG,
        )
        .then_some(bytes),
        stall_notice: STALL_NOTICE,
    }
}

#[cfg(test)]
#[path = "options_tests.rs"]
mod tests;
