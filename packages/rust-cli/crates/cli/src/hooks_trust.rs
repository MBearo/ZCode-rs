//! `zcode hooks trust`（docs/specs/rust-cli/config.md §4.3）：与 Node CLI 同形
//! （`apps/zcode-cli/packages/cli/src/hooks-trust-command.ts`、`bootstrap/src/workspace-hook-trust-cli.ts`），
//! 文本与错误逐字取 Node。信任记录与 Node CLI、App 共用。

use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::machine::Files;
use rust_cli_paths::StoragePaths;
use rust_cli_platform::{TrustLoad, TrustRecord};
use serde_json::json;

use crate::config::trust::WorkspaceHooks;
use crate::config::{ConfigInputs, read_files};

pub(crate) const USAGE: &str = "Usage:
  zcode hooks trust status [--workspace <path-or-identity>] [--json]
  zcode hooks trust review [--workspace <path-or-identity>] [--json]
  zcode hooks trust grant --workspace <path-or-identity> --hook-digest <sha256> [--hook-digest <sha256> ...]
  zcode hooks trust grant --workspace <path-or-identity> --all-current --bundle-digest <sha256>
  zcode hooks trust revoke --workspace <path-or-identity> [--hook-digest <sha256> ... | --all]
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Status,
    Review,
    Grant,
    Revoke,
}

impl Action {
    fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Review => "review",
            Self::Grant => "grant",
            Self::Revoke => "revoke",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Options {
    workspace: Option<String>,
    digests: Vec<String>,
    all_current: bool,
    bundle: Option<String>,
    all: bool,
    pub(crate) json: bool,
}

/// 解析出的命令；`Help` 打印用法。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TrustCommand {
    Help,
    Run(Action, Options),
}

/// `zcode hooks …`：不是 hooks 命令时为 None；错误是写到 stderr 的完整文本（Node `fail`）。
pub(crate) fn parse(args: &[String]) -> Option<Result<TrustCommand, String>> {
    if args.first().map(String::as_str) != Some("hooks") {
        return None;
    }
    let fail = |message: String| Err(format!("{message}\n{USAGE}"));
    if args.get(1).map(String::as_str) != Some("trust") {
        let name = args.get(1).map_or("", String::as_str);
        return Some(fail(format!("Unknown hooks command: {name}")));
    }
    let mut options = Options::default();
    let mut positionals = Vec::new();
    let mut help = false;
    let mut rest = args[2..].iter();
    while let Some(arg) = rest.next() {
        let mut value = |name: &str| {
            rest.next()
                .cloned()
                .ok_or_else(|| format!("Option '--{name} <value>' argument missing"))
        };
        let parsed = match arg.as_str() {
            "--workspace" => value("workspace").map(|v| options.workspace = Some(v)),
            "--hook-digest" => value("hook-digest").map(|v| options.digests.push(v)),
            "--bundle-digest" => value("bundle-digest").map(|v| options.bundle = Some(v)),
            "--all-current" => {
                options.all_current = true;
                Ok(())
            }
            "--all" => {
                options.all = true;
                Ok(())
            }
            "--json" => {
                options.json = true;
                Ok(())
            }
            "--help" | "-h" => {
                help = true;
                Ok(())
            }
            other if other.starts_with('-') => Err(format!("Unknown option '{other}'")),
            other => {
                positionals.push(other.to_owned());
                Ok(())
            }
        };
        if let Err(message) = parsed {
            return Some(fail(message));
        }
    }
    if help {
        return Some(Ok(TrustCommand::Help));
    }
    let action = match positionals.first().map_or("status", String::as_str) {
        "status" => Action::Status,
        "review" => Action::Review,
        "grant" => Action::Grant,
        "revoke" => Action::Revoke,
        other => return Some(fail(format!("Unknown hooks trust command: {other}"))),
    };
    if positionals.len() > 1 {
        return Some(fail(format!(
            "Unknown hooks trust command: {}",
            positionals[0]
        )));
    }
    Some(Ok(TrustCommand::Run(action, options)))
}

/// 运行需要的事实与端口。
pub(crate) struct TrustContext<'a> {
    pub(crate) files: &'a dyn Files,
    pub(crate) paths: &'a StoragePaths,
    pub(crate) cwd: &'a str,
    pub(crate) version: &'a str,
    pub(crate) now_ms: u64,
}

/// 命令的输出：退出码、stdout、stderr。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Output {
    pub(crate) code: u8,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

/// Node `path.resolve(cwd, value)`：规则是 kernel-api `env_path`（R1-9：以前自己判断绝对路径、自己拼接）。
fn join(base: &str, path: &str) -> String {
    PathStyle::infer(base).resolve(base, path, None)
}

/// Node `resolveTarget`：`local:` 等前缀是身份，其余是相对 cwd 的路径。
fn target(options: &Options, cwd: &str) -> (String, Option<String>) {
    let selected = options
        .workspace
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let identity = selected.filter(|value| {
        ["local:", "remote:", "ssh:", "container:", "wsl:"]
            .iter()
            .any(|prefix| value.starts_with(prefix))
    });
    match (identity, selected) {
        (Some(identity), _) => (cwd.to_owned(), Some(identity.to_owned())),
        (None, Some(path)) => (join(cwd, path), None),
        (None, None) => (cwd.to_owned(), None),
    }
}

async fn inspect_workspace(context: &TrustContext<'_>, options: &Options) -> WorkspaceHooks {
    let (path, identity) = target(options, context.cwd);
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new(crate::LOCAL_ENVIRONMENT),
        root: EnvPath::new(path.clone()),
    };
    let inputs = ConfigInputs {
        files: context.files,
        workspace: &workspace,
        paths: context.paths,
        process_env: &[],
        flag: None,
    };
    let files = read_files(&inputs).await;
    let mut warnings = Vec::new();
    let mut hooks = WorkspaceHooks::inspect(
        &path,
        files.user.object(),
        &files.projects,
        context.paths,
        &mut warnings,
    )
    .await;
    if let Some(identity) = identity {
        hooks.identity = identity;
    }
    hooks
}

#[path = "hooks_trust_status.rs"]
mod status;

pub(crate) use status::run;

#[cfg(test)]
#[path = "hooks_trust_tests.rs"]
mod tests;

/// grant 的记录（Node `grantWorkspaceHookTrust` 的对象字面量）。
fn records(
    hooks: &WorkspaceHooks,
    selected: &[usize],
    context: &TrustContext<'_>,
) -> Vec<TrustRecord> {
    let bundle = hooks
        .bundle
        .as_ref()
        .map(|bundle| bundle.bundle_digest.clone());
    selected
        .iter()
        .filter_map(|index| hooks.entries().get(*index))
        .map(|entry| TrustRecord {
            workspace_identity: hooks.identity.clone(),
            hook_declaration_digest: entry.digest.clone(),
            granted_at: rust_cli_base::js::iso_timestamp(context.now_ms),
            last_used_at: None,
            bundle_digest_at_grant: bundle.clone(),
            event_at_grant: entry.event.as_str().to_owned(),
            display_command_at_grant: entry.hook.display(),
            source_path_at_grant: entry.source_relative_path.clone(),
            source_discovery_order_at_grant: Some(entry.source_discovery_order as u64),
            matcher_at_grant: Some(entry.matcher.clone()),
            matcher_index_at_grant: Some(entry.matcher_index as u64),
            hook_index_at_grant: Some(entry.hook_index as u64),
            app_version_at_grant: Some(context.version.to_owned()),
        })
        .collect()
}

/// `{accepted: false, reasonCode}` 或 `Error: …`。
fn failure(options: &Options, reason: &str) -> Output {
    if options.json {
        let body = serde_json::to_string_pretty(&json!({"accepted": false, "reasonCode": reason}))
            .unwrap_or_default();
        Output {
            code: 1,
            stdout: format!("{body}\n"),
            stderr: String::new(),
        }
    } else {
        Output {
            code: 1,
            stdout: String::new(),
            stderr: format!("Error: {reason}\n"),
        }
    }
}

fn unique(values: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for value in values {
        if !out.contains(value) {
            out.push(value.clone());
        }
    }
    out
}

fn corrupt(hooks: &WorkspaceHooks) -> bool {
    matches!(hooks.load, TrustLoad::Corrupt)
}
