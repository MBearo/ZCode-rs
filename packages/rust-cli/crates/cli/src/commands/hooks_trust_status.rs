//! `zcode hooks trust` 的状态、输出与 grant / revoke（Node `inspectWorkspaceHookTrust`、
//! `grantWorkspaceHookTrust`、`revokeWorkspaceHookTrustCli`、`formatHuman`）。

use serde_json::{Map, Value, json};

use super::{
    Action, Options, Output, TrustCommand, TrustContext, USAGE, corrupt, failure,
    inspect_workspace, records, unique,
};
use crate::config::trust::WorkspaceHooks;

/// Node `WorkspaceHookTrustCliStatus`（字段顺序即 JSON 的键序）。
struct Status {
    workspace_path: String,
    identity: String,
    bundle: Option<String>,
    reason: &'static str,
    items: Vec<Item>,
}

struct Item {
    review_item_id: String,
    event: &'static str,
    matcher: Option<String>,
    display: String,
    source: String,
    enabled: bool,
    digest: String,
    trusted: bool,
}

fn status(hooks: &WorkspaceHooks, workspace_path: &str) -> Status {
    let trusted = hooks.trusted();
    let is_corrupt = corrupt(hooks);
    let items: Vec<Item> = hooks
        .entries()
        .iter()
        .map(|entry| Item {
            review_item_id: entry.review_item_id.clone(),
            event: entry.event.as_str(),
            matcher: entry.matcher.clone(),
            display: entry.hook.display(),
            source: entry.source_relative_path.clone(),
            enabled: entry.configured_enabled,
            digest: entry.digest.clone(),
            trusted: !is_corrupt && trusted.contains(&entry.digest),
        })
        .collect();
    let enabled: Vec<&Item> = items.iter().filter(|item| item.enabled).collect();
    let reason = if is_corrupt {
        "workspace_hooks_trust_store_corrupt"
    } else if hooks.bundle.is_none() {
        "workspace_hooks_not_applicable"
    } else if enabled.is_empty() {
        "workspace_hooks_no_enabled_hooks"
    } else if enabled.iter().all(|item| item.trusted) {
        "workspace_hooks_trusted_persistent"
    } else {
        "workspace_hooks_pending_trust"
    };
    Status {
        workspace_path: workspace_path.to_owned(),
        identity: hooks.identity.clone(),
        bundle: hooks.bundle.as_ref().map(|b| b.bundle_digest.clone()),
        reason,
        items,
    }
}

fn state(item: &Item) -> &'static str {
    if item.trusted {
        "trusted_persistent"
    } else {
        "pending_trust"
    }
}

fn to_json(status: &Status) -> String {
    let items: Vec<Value> = status
        .items
        .iter()
        .map(|item| {
            let mut object = Map::new();
            object.insert("reviewItemId".into(), json!(item.review_item_id));
            object.insert("event".into(), json!(item.event));
            object.insert("matcher".into(), json!(item.matcher));
            object.insert("displayCommand".into(), json!(item.display));
            object.insert("sourcePath".into(), json!(item.source));
            object.insert("configuredEnabled".into(), json!(item.enabled));
            object.insert("hookDeclarationDigest".into(), json!(item.digest));
            object.insert("trustState".into(), json!(state(item)));
            Value::Object(object)
        })
        .collect();
    let mut object = Map::new();
    object.insert("workspacePath".into(), json!(status.workspace_path));
    object.insert("workspaceIdentity".into(), json!(status.identity));
    object.insert("bundleDigest".into(), json!(status.bundle));
    object.insert("reasonCode".into(), json!(status.reason));
    object.insert("items".into(), Value::Array(items));
    format!(
        "{}\n",
        serde_json::to_string_pretty(&Value::Object(object)).unwrap_or_default()
    )
}

fn quote(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_default()
}

/// Node `formatHuman`。
fn human(status: &Status, action: Action) -> String {
    let mut lines = vec![
        format!("Workspace Hook Trust ({})", action.as_str()),
        format!("workspace: {}", status.identity),
        format!("path: {}", status.workspace_path),
        format!("bundle: {}", status.bundle.as_deref().unwrap_or("none")),
        format!("state: {}", status.reason),
    ];
    if status.items.is_empty() {
        lines.push("No Workspace Hook declarations found.".to_owned());
    }
    for (index, item) in status.items.iter().enumerate() {
        let matcher = item
            .matcher
            .as_deref()
            .map_or(String::new(), |matcher| format!(" / {matcher}"));
        let enabled = if item.enabled { "enabled" } else { "disabled" };
        lines.push(format!(
            "{}. [{}] [{enabled}] {}{matcher}",
            index + 1,
            state(item),
            item.event
        ));
        lines.push(format!("   {}", item.display));
        lines.push(format!("   source: {}", item.source));
        lines.push(format!("   digest: {}", item.digest));
    }
    if let ("workspace_hooks_pending_trust", Some(bundle)) = (status.reason, &status.bundle) {
        let workspace = quote(&status.identity);
        lines.push("Pretrust exact declarations with:".to_owned());
        lines.push(format!(
            "  zcode hooks trust grant --workspace {workspace} --hook-digest <sha256>"
        ));
        lines.push(
            "Or trust every currently enabled declaration in this exact bundle with:".to_owned(),
        );
        lines.push(format!(
            "  zcode hooks trust grant --workspace {workspace} --all-current --bundle-digest {bundle}"
        ));
    }
    if status.reason == "workspace_hooks_trust_store_corrupt" {
        lines.push(
            "The persistent trust store is corrupt; grant/revoke are rejected until it is fixed."
                .to_owned(),
        );
        lines.push(
            "The corrupted file was moved aside as workspace-hook-trust-v1.json.corrupt-<timestamp>."
                .to_owned(),
        );
        lines.push("Recovery: restore it from backup, or remove the leftover *.corrupt-* file so a fresh store is created, then re-run grant.".to_owned());
    }
    format!("{}\n", lines.join("\n"))
}

/// Node `selectGrantEntries`：返回选中的声明在 `entries()` 里的下标。
fn select(hooks: &WorkspaceHooks, options: &Options) -> Result<Vec<usize>, &'static str> {
    let digests = unique(&options.digests);
    if options.all_current != digests.is_empty() {
        return Err("Specify exactly one of --all-current or --hook-digest.");
    }
    let bundle = hooks.bundle.as_ref().map(|b| b.bundle_digest.as_str());
    if options
        .bundle
        .as_deref()
        .is_some_and(|given| Some(given) != bundle)
    {
        return Err("workspace_hooks_bundle_changed");
    }
    let entries = hooks.entries();
    if options.all_current {
        if options.bundle.is_none() {
            return Err("workspace_hooks_bundle_changed");
        }
        return Ok((0..entries.len())
            .filter(|index| entries[*index].configured_enabled)
            .collect());
    }
    let selected: Vec<usize> = (0..entries.len())
        .filter(|index| digests.contains(&entries[*index].digest))
        .collect();
    let mut found: Vec<&str> = selected
        .iter()
        .map(|i| entries[*i].digest.as_str())
        .collect();
    found.dedup();
    if found.len() != digests.len() {
        return Err("workspace_hooks_snapshot_mismatch");
    }
    Ok(selected)
}

async fn act(action: Action, options: &Options, context: &TrustContext<'_>) -> Result<(), String> {
    let hooks = inspect_workspace(context, options).await;
    match action {
        Action::Status | Action::Review => Ok(()),
        Action::Grant => {
            if hooks.bundle.is_none() {
                return Err("No Workspace Hook declarations were discovered.".to_owned());
            }
            let selected = select(&hooks, options).map_err(str::to_owned)?;
            hooks
                .store
                .grant(records(&hooks, &selected, context))
                .await
                .map_err(|error| error.to_string())
        }
        Action::Revoke => {
            let digests = unique(&options.digests);
            if options.all != digests.is_empty() {
                return Err("Specify exactly one of --all or --hook-digest.".to_owned());
            }
            let selected = (!options.all).then_some(digests.as_slice());
            hooks
                .store
                .revoke(&hooks.identity, selected)
                .await
                .map_err(|error| error.to_string())
        }
    }
}

/// 执行命令（grant / revoke 之后重新读取状态再输出，Node 相同）。
pub(crate) async fn run(command: TrustCommand, context: &TrustContext<'_>) -> Output {
    let (action, options) = match command {
        TrustCommand::Help => {
            return Output {
                code: 0,
                stdout: USAGE.to_owned(),
                stderr: String::new(),
            };
        }
        TrustCommand::Run(action, options) => (action, options),
    };
    if let Err(reason) = act(action, &options, context).await {
        return failure(&options, &reason);
    }
    let hooks = inspect_workspace(context, &options).await;
    let (path, _) = super::target(&options, context.cwd);
    let status = status(&hooks, &path);
    Output {
        code: 0,
        stdout: if options.json {
            to_json(&status)
        } else {
            human(&status, action)
        },
        stderr: String::new(),
    }
}
