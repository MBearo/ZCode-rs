//! 测试用的规则与路径事实。

use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::env_files::{PathFacts, PathStyle};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::permission::{
    PermissionMode, PermissionRules, RuleBehavior, RuleSource, SourcedRule,
};
use rust_cli_kernel_api::plan::PlanFiles;

use crate::check::{Check, Inputs};
use crate::paths::PathContext;
use crate::rules::{Rules, parse_rule};

pub const CWD: &str = "/w";

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
    let base = rules(entries);
    let rules = Rules {
        base: &base,
        session: &[],
    };
    let paths = paths();
    let inputs = Inputs {
        mode,
        rules: &rules,
        paths: &paths,
        root: CWD,
        dangerous: false,
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
