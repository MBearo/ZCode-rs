//! The prompt snapshot as Node's `EnvInfo` (`contextSnapshot.envInfo` of a
//! stored user message, Node `buildPersistedContextSnapshot` and
//! `extractPersistedEnvInfo`). Spec rust-m11-node-storage §5.2.
use crate::prompt::{GitSnapshot, PromptSnapshot};
use serde_json::{Map, Value, json};

/// Node `splitNonEmptyLines` over a rendered block.
fn lines(text: &str) -> Vec<Value> {
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| !line.is_empty())
        .map(Value::from)
        .collect()
}

impl PromptSnapshot {
    /// Node `EnvInfo` in `NodeContextSourceAdapter` key order. Rust has no
    /// `nodeVersion`; Node only reads it for display and tolerates its absence.
    pub fn env_info(&self) -> Value {
        let mut out = Map::new();
        out.insert("cwd".into(), self.cwd.clone().into());
        out.insert("platform".into(), self.platform.clone().into());
        out.insert("shell".into(), self.shell.clone().into());
        out.insert("osVersion".into(), self.os_version.clone().into());
        let Some(git) = &self.git else {
            out.insert("isGitRepository".into(), false.into());
            out.insert("gitStatus".into(), "not_repo".into());
            return Value::Object(out);
        };
        out.insert("isGitRepository".into(), true.into());
        out.insert("gitBranch".into(), git.branch.clone().into());
        for (key, value) in [("gitMainBranch", &git.main_branch), ("gitUser", &git.user)] {
            if !value.is_empty() {
                out.insert(key.into(), value.clone().into());
            }
        }
        let status = lines(&git.status);
        let state = if status.is_empty() { "clean" } else { "dirty" };
        out.insert("gitStatus".into(), state.into());
        out.insert("gitStatusLines".into(), status.into());
        out.insert("recentCommits".into(), lines(&git.recent_commits).into());
        Value::Object(out)
    }

    /// The snapshot of a resumed session: its stored `envInfo` (Node keeps it
    /// as `config.envInfo`) with the date of this runtime (Node rebuilds the
    /// context on resume).
    pub fn from_env_info(env: &Value, current_date: String) -> Self {
        let text = |key: &str| env[key].as_str().unwrap_or("").to_owned();
        // Node `isEnvInfoGitRepository`。
        let repository = env["isGitRepository"]
            .as_bool()
            .unwrap_or_else(|| match env["gitStatus"].as_str() {
                Some(status) => status != "not_repo",
                None => !text("gitBranch").is_empty(),
            });
        let joined = |key: &str| {
            env[key]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default()
        };
        // Node `formatGitStatus`：有行用行，否则 clean（Rust 以空串渲染）、dirty、unknown。
        let status = match (joined("gitStatusLines"), env["gitStatus"].as_str()) {
            (lines, _) if !lines.is_empty() => lines,
            (_, Some("clean")) => String::new(),
            (_, Some("dirty")) => "(dirty)".into(),
            _ => "(unknown)".into(),
        };
        Self {
            cwd: text("cwd"),
            platform: text("platform"),
            shell: text("shell"),
            os_version: text("osVersion"),
            current_date,
            git: repository.then(|| GitSnapshot {
                branch: text("gitBranch"),
                main_branch: text("gitMainBranch"),
                user: text("gitUser"),
                status,
                recent_commits: joined("recentCommits"),
            }),
        }
    }
}

/// Node `buildPersistedContextSnapshot`.
pub fn context_snapshot(snapshot: &PromptSnapshot) -> Value {
    json!({"envInfo": snapshot.env_info()})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(git: Option<GitSnapshot>) -> PromptSnapshot {
        PromptSnapshot {
            cwd: "/w".into(),
            platform: "darwin".into(),
            shell: "bash".into(),
            os_version: "darwin 24 arm64".into(),
            current_date: "2026-01-02".into(),
            git,
        }
    }

    #[test]
    fn env_info_round_trips_like_node() {
        let git = GitSnapshot {
            branch: "main".into(),
            main_branch: "develop".into(),
            user: String::new(),
            status: " M a.rs\n?? b.rs".into(),
            recent_commits: "abc one\ndef two".into(),
        };
        let env = snapshot(Some(git)).env_info();
        assert_eq!(
            env,
            json!({"cwd": "/w", "platform": "darwin", "shell": "bash",
                "osVersion": "darwin 24 arm64", "isGitRepository": true, "gitBranch": "main",
                "gitMainBranch": "develop", "gitStatus": "dirty",
                "gitStatusLines": [" M a.rs", "?? b.rs"], "recentCommits": ["abc one", "def two"]})
        );
        let back = PromptSnapshot::from_env_info(&env, "2026-03-04".into());
        let git = back.git.unwrap();
        assert_eq!(
            (git.status.as_str(), git.user.as_str()),
            (" M a.rs\n?? b.rs", "")
        );
        assert_eq!(back.current_date, "2026-03-04");
        let clean = json!({"isGitRepository": true, "gitStatus": "clean", "gitStatusLines": []});
        assert_eq!(
            PromptSnapshot::from_env_info(&clean, String::new())
                .git
                .unwrap()
                .status,
            ""
        );
        let none = snapshot(None).env_info();
        assert_eq!(
            (&none["isGitRepository"], &none["gitStatus"]),
            (&json!(false), &json!("not_repo"))
        );
        assert!(
            PromptSnapshot::from_env_info(&none, String::new())
                .git
                .is_none()
        );
    }
}
