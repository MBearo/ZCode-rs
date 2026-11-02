//! 测试用的规则与路径事实。

use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::env_files::{PathFacts, PathStyle};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::permission::{
    FileAccess, PermissionMode, PermissionRules, PermissionSubject, RuleBehavior, RuleSource,
    SourcedRule,
};
use rust_cli_kernel_api::plan::PlanFiles;

use crate::check::{Check, Inputs};
use crate::paths::PathContext;
use crate::rules::Rules;
use rust_cli_kernel_api::permission::syntax::parse_rule;

pub const CWD: &str = "/w";

/// 测试替身工具的判决主体：照内置工具的声明（tools 的 `permission_subject`，那边的 `permission_subject_tests.rs`
/// 逐项对照）。policy 不依赖 tools，测试里按名字给出同样的主体。
pub fn subject_of(tool: &str, input: &serde_json::Value) -> Option<PermissionSubject> {
    let text = |field: &str| input.get(field).and_then(serde_json::Value::as_str);
    let path = |access, field: &str| {
        Some(PermissionSubject::Path {
            access,
            path: text(field)
                .filter(|path| !path.is_empty())
                .map(str::to_owned),
        })
    };
    match tool {
        "Bash" => Some(PermissionSubject::Shell {
            command: text("command").unwrap_or_default().to_owned(),
        }),
        "Monitor" => input
            .get("command")
            .map(|command| PermissionSubject::Shell {
                command: command.as_str().unwrap_or_default().to_owned(),
            }),
        "Read" => path(FileAccess::Read, "file_path"),
        "Glob" | "Grep" => path(FileAccess::Read, "path"),
        "Edit" | "Write" => path(FileAccess::Write, "file_path"),
        "NotebookEdit" => path(FileAccess::Write, "notebook_path"),
        "WebFetch" => Some(PermissionSubject::Url {
            url: text("url").unwrap_or_default().to_owned(),
        }),
        "Skill" => Some(PermissionSubject::Skill {
            name: text("skill").unwrap_or_default().to_owned(),
        }),
        _ => None,
    }
}

pub fn rules(entries: &[(RuleBehavior, &str)]) -> PermissionRules {
    PermissionRules {
        version: 1,
        rules: entries
            .iter()
            .map(|(behavior, text)| SourcedRule {
                source: RuleSource::CliArg,
                behavior: *behavior,
                rule: parse_rule(text),
            })
            .collect(),
        ..PermissionRules::default()
    }
}

pub fn paths() -> PathContext {
    PathContext {
        cwd: CWD.to_owned(),
        roots: vec![CWD.to_owned()],
        home: Some("/home/dev".to_owned()),
        style: PathStyle::Posix,
        internal: vec!["/tmp/zcode-shell/sessions/s1/tasks".to_owned()],
        plans: PlanFiles::in_workspace(
            &PathFacts {
                home: Some(EnvPath::new("/home/dev")),
                style: PathStyle::Posix,
            },
            &EnvPath::new(CWD),
            &SessionId::new("s1"),
        ),
    }
}

/// 在给定模式与规则下跑一次检查。
pub fn run<T>(
    mode: PermissionMode,
    entries: &[(RuleBehavior, &str)],
    check: impl FnOnce(&Inputs<'_>) -> T,
) -> T {
    run_in(&paths(), mode, entries, check)
}

/// 同 [`run`]，路径事实由调用方给出（规则的根是第一个工作目录）。
pub fn run_in<T>(
    paths: &PathContext,
    mode: PermissionMode,
    entries: &[(RuleBehavior, &str)],
    check: impl FnOnce(&Inputs<'_>) -> T,
) -> T {
    let base = rules(entries);
    let rules = Rules {
        base: &base,
        session: &[],
    };
    let root = paths.roots.first().map_or(CWD, String::as_str);
    let inputs = Inputs {
        mode,
        rules: &rules,
        paths,
        root,
        dangerous: false,
        full_access: mode == PermissionMode::BypassPermissions,
    };
    check(&inputs)
}

pub fn message(check: &Check) -> String {
    match check {
        Check::Allow => "allow".to_owned(),
        Check::Deny { message } => format!("deny: {message}"),
        Check::Ask { message, .. } => format!("ask: {message}"),
        Check::Passthrough { reason, .. } => format!(
            "passthrough: {}",
            crate::texts::request_message("Bash", reason.as_ref())
        ),
    }
}
