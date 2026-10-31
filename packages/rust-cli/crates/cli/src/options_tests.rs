use super::*;

use rust_cli_base::hosts::BIGMODEL_ANTHROPIC;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::provider::TokenLimits;

/// 答不出的上限查询（这里只看环境变量）。
struct NoLimits;

impl ModelLimits for NoLimits {
    fn limits(&self, _selection: &ModelSelection) -> Option<TokenLimits> {
        None
    }
}

fn none() -> Arc<dyn ModelLimits> {
    Arc::new(NoLimits)
}

fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// 名字按修复线 F42 换成 `ZCODE_*`（config.md §2.5），旧名字不再读。
#[test]
fn compaction_follows_the_zcode_variables() {
    let config = compact(
        &env(&[
            ("ZCODE_AUTOCOMPACT_PCT_OVERRIDE", "50"),
            ("ZCODE_AUTO_COMPACT_WINDOW", "200k"),
        ]),
        None,
        none(),
    );
    assert!(config.auto);
    assert_eq!(config.thresholds.pct_override, Some(50.0));
    assert_eq!(config.thresholds.window_override, Some(200_000));
    assert!(config.compact_command);
    let disabled = compact(&env(&[("DISABLE_COMPACT", "1")]), None, none());
    assert!(!disabled.auto && !disabled.compact_command);
    // 只关自动压缩：`/compact` 仍能用，提示照样接“Run /compact”。
    let manual = compact(&env(&[("DISABLE_AUTO_COMPACT", "1")]), None, none());
    assert!(!manual.auto && manual.compact_command);
    assert_eq!(window("300"), Some(300_000));
    assert_eq!(window("1m"), Some(1_000_000));
    assert_eq!(window("150000"), Some(150_000));
    assert_eq!(window("auto"), None);
}

/// `ZCODE_MAX_OUTPUT_TOKENS` 也给压缩的摘要预留（compact.md §2，F42）。修复线 F58：
/// 值由入口读一次传进来（`Startup.max_output_tokens`），这里不再自己读环境变量（以前同一个变量读 4 次，无效值的
/// 警告写 4 次）。
#[test]
fn compaction_takes_the_output_limit_read_once_by_the_entry() {
    let config = compact(
        &env(&[("ZCODE_MAX_OUTPUT_TOKENS", "lots")]),
        Some(8000),
        none(),
    );
    assert_eq!(config.thresholds.max_output_tokens, Some(8000));
    let unset = compact(&env(&[("ZCODE_MAX_OUTPUT_TOKENS", "8000")]), None, none());
    assert_eq!(unset.thresholds.max_output_tokens, None);
}

#[test]
fn stream_idle_follows_the_zcode_variables() {
    use std::time::Duration;
    let secs = Duration::from_secs;
    // 缺省：两个看门狗都开，事件级 300 s，字节级按端点。
    assert_eq!(stream_idle(&env(&[])), StreamIdle::default());
    // 事件级只能调大；设了它时字节级也用它。
    let low = stream_idle(&env(&[("ZCODE_STREAM_IDLE_TIMEOUT_MS", "1000")]));
    assert_eq!(low.events, Some(secs(300)));
    assert_eq!(low.bytes, Some(ByteIdle::Fixed(secs(300))));
    let high = stream_idle(&env(&[("ZCODE_STREAM_IDLE_TIMEOUT_MS", "600000ms")]));
    assert_eq!(high.events, Some(secs(600)));
    assert_eq!(high.bytes, Some(ByteIdle::Fixed(secs(600))));
    // 字节级单独设，夹在 [10 s, 30 min]。
    let bytes =
        |value: &str| stream_idle(&env(&[("ZCODE_BYTE_STREAM_IDLE_TIMEOUT_MS", value)])).bytes;
    assert_eq!(bytes("10000"), Some(ByteIdle::Fixed(secs(10))));
    assert_eq!(bytes("2000"), Some(ByteIdle::Fixed(secs(10))));
    assert_eq!(bytes("99999999"), Some(ByteIdle::Fixed(secs(1800))));
    assert_eq!(bytes("0"), Some(ByteIdle::Endpoint));
    assert_eq!(bytes("soon"), Some(ByteIdle::Endpoint));
    // 关掉。
    let off = stream_idle(&env(&[
        ("ZCODE_ENABLE_STREAM_WATCHDOG", "false"),
        ("ZCODE_ENABLE_BYTE_WATCHDOG", "0"),
    ]));
    assert_eq!((off.events, off.bytes), (None, None));
    let on = stream_idle(&env(&[("ZCODE_ENABLE_STREAM_WATCHDOG", "1")]));
    assert_eq!(on.events, Some(secs(300)));
}

/// 任务工具的开关（R4 F6）：`ZCODE_ENABLE_TODO_TOOLS` 打开，
/// `ZCODE_ENABLE_TASKS=false` 关闭且优先；都没有时按模型。
#[test]
fn task_tools_follow_the_zcode_variables() {
    assert_eq!(task_tools(&env(&[])), None);
    assert_eq!(
        task_tools(&env(&[("ZCODE_ENABLE_TODO_TOOLS", "1")])),
        Some(true)
    );
    assert_eq!(task_tools(&env(&[("ZCODE_ENABLE_TODO_TOOLS", "0")])), None);
    assert_eq!(
        task_tools(&env(&[("ZCODE_ENABLE_TASKS", "false")])),
        Some(false)
    );
    assert_eq!(
        task_tools(&env(&[
            ("ZCODE_ENABLE_TODO_TOOLS", "true"),
            ("ZCODE_ENABLE_TASKS", "0"),
        ])),
        Some(false)
    );
    assert_eq!(task_tools(&env(&[("ZCODE_ENABLE_TASKS", "1")])), None);
}

/// 两个只给测试用的声明（model.md §6）：接受时按真值读。
#[test]
fn accepted_declarations_are_truthy_test_variables() {
    let read = |pairs: &[(&str, &str)]| {
        let snapshot = env(pairs);
        assumptions(TestDeclarations::Accepted, |name| {
            var(&snapshot, name).map(str::to_owned)
        })
    };
    assert_eq!(read(&[]), Assumptions::default());
    let both = read(&[
        ("_ZCODE_ASSUME_FIRST_PARTY_BASE_URL", "1"),
        ("_ZCODE_ASSUME_TOOL_SEARCH", "TRUE"),
    ]);
    assert!(both.first_party && both.tool_search, "{both:?}");
    let first_party = read(&[("_ZCODE_ASSUME_FIRST_PARTY_BASE_URL", "yes")]);
    assert!(first_party.first_party && !first_party.tool_search);
    let tool_search = read(&[("_ZCODE_ASSUME_TOOL_SEARCH", "on")]);
    assert!(!tool_search.first_party && tool_search.tool_search);
    assert_eq!(
        read(&[
            ("_ZCODE_ASSUME_FIRST_PARTY_BASE_URL", "0"),
            ("_ZCODE_ASSUME_TOOL_SEARCH", "false"),
        ]),
        Assumptions::default()
    );
}

/// 按另一个地址查方案表与错误码表的测试声明（provider-errors.md D13）：取值是地址，空白当没设；不接受时不读。
#[test]
fn the_assumed_base_url_is_a_test_declaration() {
    let snapshot = env(&[("_ZCODE_ASSUME_BASE_URL", BIGMODEL_ANTHROPIC)]);
    assert_eq!(
        assumed_base_url(TestDeclarations::Accepted, |name| {
            var(&snapshot, name).map(str::to_owned)
        }),
        Some(BIGMODEL_ANTHROPIC.to_owned())
    );
    let blank = env(&[("_ZCODE_ASSUME_BASE_URL", "  ")]);
    assert_eq!(
        assumed_base_url(TestDeclarations::Accepted, |name| {
            var(&blank, name).map(str::to_owned)
        }),
        None
    );
    let reads = std::cell::Cell::new(0);
    let ignored = assumed_base_url(TestDeclarations::Ignored, |_| {
        reads.set(reads.get() + 1);
        Some(BIGMODEL_ANTHROPIC.to_owned())
    });
    assert_eq!((ignored, reads.get()), (None, 0));
}

/// 工作线 F52b（用户决定 2026-10-22）：不接受测试声明（release 构建）时设了也不生效，变量一个都不读。
#[test]
fn ignored_declarations_are_not_read() {
    let reads = std::cell::Cell::new(0);
    let found = assumptions(TestDeclarations::Ignored, |_| {
        reads.set(reads.get() + 1);
        Some("1".to_owned())
    });
    assert_eq!(found, Assumptions::default());
    assert_eq!(reads.get(), 0);
}

/// 本次构建的取值：debug 构建接受，release 构建忽略（入口只用它）。
#[test]
fn the_build_accepts_declarations_only_in_debug() {
    let expected = if cfg!(debug_assertions) {
        TestDeclarations::Accepted
    } else {
        TestDeclarations::Ignored
    };
    assert_eq!(TestDeclarations::BUILD, expected);
}

/// `IS_SANDBOX`：非空就是真（`0` 也是），cli-args.md §9。
#[test]
fn any_non_empty_sandbox_value_counts() {
    let env = |value: &str| vec![("IS_SANDBOX".to_owned(), value.to_owned())];
    assert!(is_sandbox(&env("1")));
    assert!(is_sandbox(&env("0")));
    assert!(!is_sandbox(&env("")));
    assert!(!is_sandbox(&[]));
}

/// `IS_SANDBOX` 的两种读法并列（F58：名字与读法收在 options.rs；`refuse_root_bypass` 用第二种）。
#[test]
fn the_two_sandbox_readings_differ_by_site() {
    let env = |value: &str| vec![("IS_SANDBOX".to_owned(), value.to_owned())];
    for (value, retries, root) in [
        ("1", true, true),
        ("true", true, false),
        ("0", true, false),
        ("", false, false),
    ] {
        assert_eq!(is_sandbox(&env(value)), retries, "{value:?}");
        assert_eq!(is_sandbox_one(&env(value)), root, "{value:?}");
    }
    assert!(!is_sandbox_one(&[]));
}

/// 工作线 F83：Monitor 产品缺省开；只给测试用的声明关掉（只在接受声明时读），Windows 上还要找到 Git Bash。
#[test]
fn monitor_is_on_unless_declared_off() {
    let read = |pairs: &[(&str, &str)], declarations| {
        let snapshot = env(pairs);
        assumptions(declarations, |name| var(&snapshot, name).map(str::to_owned))
    };
    let plain = read(&[], TestDeclarations::Accepted);
    assert!(monitor(&plain, true));
    assert_eq!(monitor(&plain, false), !cfg!(windows));
    let off = read(
        &[("_ZCODE_ASSUME_MONITOR_OFF", "1")],
        TestDeclarations::Accepted,
    );
    assert!(off.monitor_off && !monitor(&off, true));
    let release = read(
        &[("_ZCODE_ASSUME_MONITOR_OFF", "1")],
        TestDeclarations::Ignored,
    );
    assert!(monitor(&release, true));
}

/// 远端执行 R1（remote-exec.md §18.6）：loopback 的声明只在接受声明时读（release 构建不认），缺省是本机的执行环境。
#[test]
fn the_loopback_declaration_is_read_only_when_declarations_are_accepted() {
    let read = |pairs: &[(&str, &str)], declarations| {
        let snapshot = env(pairs);
        assumptions(declarations, |name| var(&snapshot, name).map(str::to_owned))
    };
    assert!(!read(&[], TestDeclarations::Accepted).loopback);
    let declared = [("_ZCODE_ASSUME_LOOPBACK_EXECUTOR", "1")];
    assert!(read(&declared, TestDeclarations::Accepted).loopback);
    assert!(!read(&declared, TestDeclarations::Ignored).loopback);
    let off = [("_ZCODE_ASSUME_LOOPBACK_EXECUTOR", "0")];
    assert!(!read(&off, TestDeclarations::Accepted).loopback);
}

/// 远端执行 R3（remote-exec.md §20.10）：loopback 机器的声明只在接受声明时读（release 构建不认）。
#[test]
fn the_loopback_machines_declaration_is_read_only_when_declarations_are_accepted() {
    let read = |pairs: &[(&str, &str)], declarations| {
        let snapshot = env(pairs);
        assumptions(declarations, |name| var(&snapshot, name).map(str::to_owned))
    };
    assert!(!read(&[], TestDeclarations::Accepted).loopback_machines);
    let declared = [("_ZCODE_ASSUME_LOOPBACK_MACHINES", "1")];
    assert!(read(&declared, TestDeclarations::Accepted).loopback_machines);
    assert!(!read(&declared, TestDeclarations::Ignored).loopback_machines);
    // 两个 loopback 声明互不影响。
    assert!(!read(&declared, TestDeclarations::Accepted).loopback);
    let ssh = [("_ZCODE_ASSUME_SSH_MACHINES", "1")];
    assert!(read(&ssh, TestDeclarations::Accepted).ssh_machines);
    assert!(!read(&ssh, TestDeclarations::Ignored).ssh_machines);
    // 机器经 SSH 时的测试配置：不要求 `_ZCODE_ASSUME_SSH_EXECUTOR`；release 构建什么都不读。
    // F100（remote-exec.md §26.7）：执行器的假注册表也只在 debug 构建读。
    let vars = env(&[
        ("_ZCODE_ASSUME_SSH_CONFIG", "/tmp/ssh_config"),
        ("_ZCODE_ASSUME_NPM_REGISTRY", "http://127.0.0.1:9"),
    ]);
    assert_eq!(
        machine_ssh(TestDeclarations::Accepted, |name| var(&vars, name)
            .map(str::to_owned)),
        MachineSsh {
            config: Some("/tmp/ssh_config".to_owned()),
            artifact: None,
            npm_registry: Some("http://127.0.0.1:9".to_owned()),
        }
    );
    assert_eq!(
        machine_ssh(TestDeclarations::Ignored, |_: &str| -> Option<String> {
            panic!("release builds do not read the variables")
        }),
        MachineSsh::default()
    );
}

/// 远端执行 R2（remote-exec.md §19.11）：SSH 的测试入口只在接受声明时读，要有目标；release 构建不读任何一个变量。
#[test]
fn the_ssh_entry_is_read_only_when_declarations_are_accepted() {
    let vars = [
        ("_ZCODE_ASSUME_SSH_EXECUTOR", "test-host"),
        ("_ZCODE_ASSUME_SSH_CONFIG", "/tmp/ssh_config"),
        ("_ZCODE_ASSUME_SSH_ARTIFACT", ""),
    ];
    let lookup = |name: &str| {
        vars.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_owned())
    };
    assert_eq!(
        ssh_assumption(TestDeclarations::Accepted, lookup),
        Some(SshAssumption {
            destination: "test-host".to_owned(),
            config: Some("/tmp/ssh_config".to_owned()),
            artifact: None,
        })
    );
    assert_eq!(
        ssh_assumption(TestDeclarations::Ignored, |_: &str| -> Option<String> {
            panic!("release builds do not read the variables")
        }),
        None
    );
    assert_eq!(
        ssh_assumption(TestDeclarations::Accepted, |_: &str| None),
        None
    );
}
