//! Workspace-scoped settings (Node `local_setting` with scope `project`).
//!
//! Only the engine writes these rows, through the storage worker, so a value read
//! at startup stays current for the whole runtime.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::collections::BTreeMap;
use tokio::sync::oneshot;

type Settings = BTreeMap<(String, String), Value>;

pub(super) enum Request {
    Load(String, oneshot::Sender<Result<Settings>>),
    Save {
        workspace: String,
        namespace: String,
        key: String,
        value: String,
        reply: oneshot::Sender<Result<()>>,
    },
}

/// Node imports these keys from `local_setting` (namespace, key).
const IMPORTED: [(&str, &str); 2] = [("permission", "ruleset"), ("permission", "mode")];

pub(super) fn prepare(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS rust_project_setting(workspace TEXT NOT NULL,namespace TEXT NOT NULL,key TEXT NOT NULL,value TEXT NOT NULL,PRIMARY KEY(workspace,namespace,key));")?;
    Ok(())
}

pub(super) fn handle(conn: &Connection, request: Request) {
    match request {
        Request::Load(workspace, reply) => {
            let _ = reply.send(load(conn, &workspace));
        }
        Request::Save {
            workspace,
            namespace,
            key,
            value,
            reply,
        } => {
            let _ = reply.send(
                conn.execute(
                    "INSERT INTO rust_project_setting VALUES(?1,?2,?3,?4) ON CONFLICT(workspace,namespace,key) DO UPDATE SET value=excluded.value",
                    params![workspace, namespace, key, value],
                )
                .map(drop)
                .map_err(Into::into),
            );
        }
    }
}

fn load(conn: &Connection, workspace: &str) -> Result<Settings> {
    let mut query = conn.prepare_cached(
        "SELECT namespace,key,value FROM rust_project_setting WHERE workspace=?1",
    )?;
    let rows = query.query_map([workspace], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut settings = Settings::new();
    for row in rows {
        let (namespace, key, value) = row?;
        // Node `decodeJson` 把损坏的值当作不存在，不因一条坏偏好阻止 runtime 启动。
        if let Ok(value) = serde_json::from_str(&value) {
            settings.insert((namespace, key), value);
        }
    }
    Ok(settings)
}

/// Node `projectIdFromDirectory`.
fn project_id(directory: &str) -> String {
    let mut slug = String::new();
    let mut replaced = false;
    // 与 `/[^a-z0-9._-]+/g` 一致：只折叠被替换的连续字符，原有的 `-` 原样保留。
    for c in directory.to_lowercase().chars() {
        let kept = c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-');
        if kept {
            slug.push(c);
        } else if !replaced {
            slug.push('-');
        }
        replaced = !kept;
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "session" } else { slug };
    slug.chars().take(80).collect()
}

/// Copies the project's Node settings; rows Rust already has win.
pub(super) fn import(
    dest: &Connection,
    snapshot: &Connection,
    workspace: &str,
    cwd: &str,
) -> Result<()> {
    let exists: bool = snapshot.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='local_setting')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(());
    }
    let project = project_id(cwd);
    for (namespace, key) in IMPORTED {
        let value: Option<String> = snapshot
            .query_row(
                "SELECT value FROM local_setting WHERE scope='project' AND scope_id=?1 AND namespace=?2 AND key=?3",
                params![project, namespace, key],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(value) = value {
            dest.execute(
                "INSERT OR IGNORE INTO rust_project_setting VALUES(?1,?2,?3,?4)",
                params![workspace, namespace, key, value],
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_ids_follow_node_slugify() {
        assert_eq!(project_id("/Users/Me/My Project"), "users-me-my-project");
        assert_eq!(project_id("C:\\Work\\app.v2"), "c-work-app.v2");
        assert_eq!(project_id("/"), "session");
        assert_eq!(project_id("/a- b"), "a--b");
        assert_eq!(project_id("/项目/a"), "a");
        assert_eq!(project_id(&format!("/{}", "a".repeat(100))).len(), 80);
    }

    #[test]
    fn import_copies_node_permission_rows_without_overwriting() -> Result<()> {
        let source = Connection::open_in_memory()?;
        source.execute_batch("CREATE TABLE local_setting(scope TEXT,scope_id TEXT,namespace TEXT,key TEXT,value TEXT);
            INSERT INTO local_setting VALUES('project','tmp-w','permission','ruleset','{\"version\":1,\"allow\":[{\"toolName\":\"Write\"}]}');
            INSERT INTO local_setting VALUES('project','tmp-w','permission','mode','{\"mode\":\"edit\"}');
            INSERT INTO local_setting VALUES('project','other','permission','mode','{\"mode\":\"yolo\"}');")?;
        let dest = Connection::open_in_memory()?;
        prepare(&dest)?;
        dest.execute(
            "INSERT INTO rust_project_setting VALUES('w','permission','mode','{\"mode\":\"build\"}')",
            [],
        )?;
        import(&dest, &source, "w", "/tmp/w")?;
        let settings = load(&dest, "w")?;
        assert_eq!(
            settings[&("permission".into(), "mode".into())]["mode"],
            "build"
        );
        assert_eq!(
            settings[&("permission".into(), "ruleset".into())]["allow"][0]["toolName"],
            "Write"
        );
        Ok(())
    }
}
