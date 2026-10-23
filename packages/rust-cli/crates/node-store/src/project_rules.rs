//! 本机项目规则集（docs/specs/rust-cli/shell-permissions.md §5）：Node
//! `repositories/local-settings.ts` 的 `getProjectPermission` / `saveProjectPermission` 与
//! `core/src/tool/executor/permission-rules.ts` 的 `applyPermissionUpdates`。
//!
//! 值存在 `local_setting(scope='project', scope_id=<projectId>, namespace='permission', key='ruleset')`，
//! 没有这一行时读旧表 `permission(project_id)`。值是 `{version: 1, allow?, deny?, ask?, …}`，规则是
//! `{toolName, ruleContent?}`。写入只改 `local_setting`（Node 相同），键序同 Node 的对象展开。
//!
//! Node 群协作会话（修复线 F2，Node `loadProjectPermissionRuleset` / `persistProjectPermissionUpdates`）：会话行的
//! `permission.scope` 是 `session` 时，规则集就是会话行的 `permission`（同样的键），“总是允许”只写它，不读也不写
//! 项目规则集。以前续写这样的会话时规则写进项目、读的也是项目的。

use rusqlite::{Connection, OptionalExtension, params};
use rust_cli_base::js::json::stringify;
use rust_cli_kernel_api::permission::{PermissionRule, ProjectRules, RuleBehavior};
use rust_cli_kernel_api::store::StoreError;
use serde_json::{Map, Value, json};

use crate::facts::Commit;
use crate::{OrStorage, storage};

/// Node 存的值（JSON 文本）：`local_setting` 优先，没有时读旧 `permission` 表；都没有时为 None。
fn stored(conn: &Connection, project: &str) -> rusqlite::Result<Option<String>> {
    let setting: Option<String> = conn
        .prepare_cached(
            "select value from local_setting where scope = ? and scope_id = ? and namespace = ? and key = ?",
        )?
        .query_row(params!["project", project, "permission", "ruleset"], |row| {
            row.get(0)
        })
        .optional()?;
    if setting.is_some() {
        return Ok(setting);
    }
    conn.prepare_cached("select data from permission where project_id = ?")?
        .query_row(params![project], |row| row.get(0))
        .optional()
}

fn rules(value: &Value, key: &str) -> Vec<PermissionRule> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let tool = item.get("toolName")?.as_str()?.to_owned();
                    let content = item
                        .get("ruleContent")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    Some(PermissionRule { tool, content })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 读本项目的规则集；值读不出时按空处理（写入时才报错，不覆盖）。
pub(crate) fn read(conn: &Connection, project: &str) -> Result<ProjectRules, StoreError> {
    let Some(text) = stored(conn, project).storage()? else {
        return Ok(ProjectRules::default());
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Ok(ProjectRules::default());
    };
    Ok(parsed(&value))
}

fn parsed(value: &Value) -> ProjectRules {
    ProjectRules {
        allow: rules(value, "allow"),
        deny: rules(value, "deny"),
        ask: rules(value, "ask"),
    }
}

/// 会话行的 `permission`，是 Node 群协作授权（`scope = "session"`）时为 Some。
pub(crate) fn scoped(permission: Option<&Value>) -> Option<&Map<String, Value>> {
    permission?
        .as_object()
        .filter(|permission| permission.get("scope").and_then(Value::as_str) == Some("session"))
}

/// Node 群协作会话自己的规则集（`SessionStore::session_rules`）；其他会话为 None。
pub(crate) fn session_read(
    conn: &Connection,
    session: &str,
) -> Result<Option<ProjectRules>, StoreError> {
    let row = crate::repo::sessions::get(conn, session).storage()?;
    Ok(row
        .as_ref()
        .and_then(|row| scoped(row.permission.as_ref()))
        .map(|permission| parsed(&Value::Object(permission.clone()))))
}

fn behavior_key(behavior: RuleBehavior) -> &'static str {
    match behavior {
        RuleBehavior::Allow => "allow",
        RuleBehavior::Deny => "deny",
        RuleBehavior::Ask => "ask",
    }
}

fn node_rule(rule: &PermissionRule) -> Value {
    let mut object = Map::new();
    object.insert("toolName".into(), json!(rule.tool));
    if let Some(content) = &rule.content {
        object.insert("ruleContent".into(), json!(content));
    }
    Value::Object(object)
}

/// Node 的去重键 `toolName + "\0" + (ruleContent ?? "")`。
fn rule_key(rule: &Value) -> String {
    let name = rule
        .get("toolName")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let content = rule
        .get("ruleContent")
        .and_then(Value::as_str)
        .unwrap_or_default();
    format!("{name}\u{0}{content}")
}

/// Node `applyPermissionUpdates`：`{...当前, version: 1}`，再把规则追加到 `behavior` 键并去重（保留先出现的）。
/// `removed` 是 `/permissions` 的删除（Node 没有删除，按同一个去重键去掉，tui-manage.md §1.1）：先删再加。
fn apply(
    current: Map<String, Value>,
    behavior: RuleBehavior,
    added: &[PermissionRule],
    removed: &[PermissionRule],
) -> Value {
    let mut next = current;
    next.insert("version".into(), json!(1));
    let key = behavior_key(behavior);
    let existing = next
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let gone: std::collections::HashSet<String> = removed
        .iter()
        .map(|rule| rule_key(&node_rule(rule)))
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut deduped = Vec::new();
    for rule in existing
        .into_iter()
        .filter(|rule| !gone.contains(&rule_key(rule)))
        .chain(added.iter().map(node_rule))
    {
        if seen.insert(rule_key(&rule)) {
            deduped.push(rule);
        }
    }
    next.insert(key.into(), Value::Array(deduped));
    Value::Object(next)
}

/// 在提交事务里把规则加进本会话所在项目的规则集（Node `persistProjectPermissionUpdates`）。当前值读不出时
/// 整批失败，不覆盖（Node 在这里抛错，这次授权没有记住）。
pub(crate) fn add(
    ctx: &Commit<'_>,
    behavior: RuleBehavior,
    added: &[PermissionRule],
) -> Result<(), StoreError> {
    let row = crate::repo::sessions::get(ctx.conn, ctx.session).storage()?;
    if let Some(permission) = row.as_ref().and_then(|row| scoped(row.permission.as_ref())) {
        let update = crate::repo::sessions::Update {
            id: ctx.session.to_owned(),
            permission: Some(apply(permission.clone(), behavior, added, &[])),
            ..Default::default()
        };
        crate::repo::sessions::update(ctx.conn, &update, ctx.now as i64).storage()?;
        return Ok(());
    }
    let project = session_project(ctx)?;
    let current = current_object(ctx.conn, &project)?;
    let next = apply(current, behavior, added, &[]);
    write_setting(ctx, &project, "ruleset", &next)
}

/// 当前规则集的 JSON 对象；没有时为空对象，不是对象时报错（不覆盖）。
fn current_object(conn: &Connection, project: &str) -> Result<Map<String, Value>, StoreError> {
    match stored(conn, project).storage()? {
        None => Ok(Map::new()),
        Some(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(object)) => Ok(object),
            Ok(_) | Err(_) => Err(storage(format!(
                "the permission ruleset of {project} is not a JSON object"
            ))),
        },
    }
}

/// `/permissions` 的修改（tui-manage.md §1.1）：不经会话，在一个 `begin immediate` 事务里读、改、upsert，
/// 返回落库后的规则集。
pub(crate) fn change(
    conn: &mut Connection,
    project: &str,
    now: u64,
    change: &rust_cli_kernel_api::store::ProjectRulesChange,
) -> Result<ProjectRules, StoreError> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .storage()?;
    let current = current_object(&tx, project)?;
    let next = apply(current, change.behavior, &change.add, &change.remove);
    upsert(&tx, now, project, "ruleset", &next)?;
    let rules = read(&tx, project)?;
    tx.commit().storage()?;
    Ok(rules)
}

/// 本会话所在项目的 ID（会话行的 `project_id`）。
pub(crate) fn session_project(ctx: &Commit<'_>) -> Result<String, StoreError> {
    ctx.conn
        .prepare_cached("select project_id from session where id = ?")
        .storage()?
        .query_row(params![ctx.session], |row| row.get(0))
        .storage()
}

/// Node `writeLocalSetting`：`local_setting(project, <项目>, permission, <key>)` 的 upsert，
/// `schema_version` 1，已有行只改 value、schema_version 与 time_updated。
pub(crate) fn write_setting(
    ctx: &Commit<'_>,
    project: &str,
    key: &str,
    value: &Value,
) -> Result<(), StoreError> {
    upsert(ctx.conn, ctx.now, project, key, value)
}

fn upsert(
    conn: &Connection,
    now: u64,
    project: &str,
    key: &str,
    value: &Value,
) -> Result<(), StoreError> {
    let now = i64::try_from(now).unwrap_or(i64::MAX);
    conn
        .prepare_cached(
            "insert into local_setting (scope, scope_id, namespace, key, value, schema_version, time_created, time_updated)
             values (?, ?, ?, ?, ?, ?, ?, ?)
             on conflict(scope, scope_id, namespace, key) do update set
               value = excluded.value,
               schema_version = excluded.schema_version,
               time_updated = excluded.time_updated",
        )
        .storage()?
        .execute(params![
            "project",
            project,
            "permission",
            key,
            stringify(value),
            1,
            now,
            now
        ])
        .storage()?;
    Ok(())
}

#[cfg(test)]
#[path = "project_rules_tests.rs"]
mod tests;
