//! Marketplace source strings (Node `parseMarketplaceSourceInput`). Spec
//! rust-m10-4-plugin-sources §4.1.
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Node `splitRef`: the last `#` separates a ref.
fn split_ref(input: &str) -> (&str, Option<&str>) {
    match input.rfind('#') {
        Some(at) => (&input[..at], Some(&input[at + 1..])),
        None => (input, None),
    }
}

/// Node `splitGitHubShorthand`: the last `#` or `@` (not at the start).
fn split_shorthand(input: &str) -> (&str, Option<&str>) {
    match input.rfind(['#', '@']).filter(|at| *at > 0) {
        Some(at) => (&input[..at], Some(&input[at + 1..])),
        None => (input, None),
    }
}

fn git(url: &str, reference: Option<&str>) -> Value {
    let mut source = json!({"source":"git","url":url});
    if let Some(reference) = reference {
        source["ref"] = reference.into();
    }
    source
}

/// Node `isGitSshUrl`: `^[a-zA-Z0-9._-]+@[^:]+:.+`.
fn git_ssh(input: &str) -> bool {
    let Some(at) = input.find('@') else {
        return false;
    };
    let user = &input[..at];
    let valid_user = !user.is_empty()
        && user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    let rest = &input[at + 1..];
    valid_user
        && rest
            .find(':')
            .is_some_and(|colon| colon > 0 && rest[colon + 1..].chars().any(|c| c != '\n'))
}

/// A GitHub web URL naming `owner/repo` (Node's pathname match).
fn github_repository(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    if !matches!(parsed.host_str(), Some("github.com" | "www.github.com")) {
        return false;
    }
    let Some(path) = parsed.path().strip_prefix('/') else {
        return false;
    };
    path.split_once('/').is_some_and(|(owner, repo)| {
        !owner.is_empty() && !repo.is_empty() && !repo.starts_with('/')
    })
}

/// Node `resolvePathInput`: relative to the process working directory,
/// `~` against `HOME`.
fn path_input(input: &str, home: &str) -> Option<PathBuf> {
    let drive = input.len() >= 3
        && input.as_bytes()[0].is_ascii_alphabetic()
        && input.as_bytes()[1] == b':'
        && matches!(input.as_bytes()[2], b'/' | b'\\');
    if !(input.starts_with("./")
        || input.starts_with("../")
        || input.starts_with('/')
        || input.starts_with('~')
        || drive)
    {
        return None;
    }
    if let Some(rest) = input.strip_prefix('~') {
        // Node `join(HOME ?? "", rest)`：空段被忽略，其余以分隔符拼接后规范化。
        let joined = match (home.is_empty(), rest.is_empty()) {
            (true, true) => ".".to_owned(),
            (true, false) => rest.to_owned(),
            (false, true) => home.to_owned(),
            (false, false) => format!("{home}/{rest}"),
        };
        return Some(crate::fsx::normalize(Path::new(&joined)));
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    Some(crate::fsx::resolve(&cwd, input))
}

pub async fn parse(input: &str, home: &str) -> Result<Value> {
    let trimmed = crate::js::trim(input);
    if trimmed.is_empty() {
        bail!("Marketplace source is empty");
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        let (url, reference) = split_ref(trimmed);
        if url.ends_with(".git") || url.contains("/_git/") {
            return Ok(git(url, reference));
        }
        if github_repository(url) {
            return Ok(git(&format!("{url}.git"), reference));
        }
        return Ok(json!({"source":"url","url":url}));
    }
    if git_ssh(trimmed) {
        let (url, reference) = split_ref(trimmed);
        return Ok(git(url, reference));
    }
    if let Some(resolved) = path_input(trimmed, home) {
        let shown = resolved.display();
        let Ok(metadata) = tokio::fs::metadata(&resolved).await else {
            bail!("Marketplace source path does not exist: {shown}");
        };
        if metadata.is_file() {
            if !resolved.to_string_lossy().ends_with(".json") {
                bail!("Marketplace file must be a .json file: {shown}");
            }
            return Ok(json!({"source":"file","path":resolved.to_string_lossy()}));
        }
        if metadata.is_dir() {
            return Ok(json!({"source":"directory","path":resolved.to_string_lossy()}));
        }
        bail!("Marketplace source path is not a file or directory: {shown}");
    }
    if trimmed.contains('/') && !trimmed.contains(':') {
        let (repo, reference) = split_shorthand(trimmed);
        let mut source = json!({"source":"github","repo":repo});
        if let Some(reference) = reference {
            source["ref"] = reference.into();
        }
        return Ok(source);
    }
    bail!("Unsupported marketplace source: {input}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn source_strings_follow_node() {
        let parse = |s: &'static str| async move { parse(s, "/home/u").await };
        assert_eq!(
            parse("https://github.com/o/r#v1").await.unwrap(),
            json!({"source":"git","url":"https://github.com/o/r.git","ref":"v1"})
        );
        assert_eq!(
            parse("https://host/x/_git/r").await.unwrap(),
            json!({"source":"git","url":"https://host/x/_git/r"})
        );
        assert_eq!(
            parse(" https://cdn.test/m.json#x ").await.unwrap(),
            json!({"source":"url","url":"https://cdn.test/m.json"})
        );
        assert_eq!(
            parse("git@host:org/r.git#main").await.unwrap(),
            json!({"source":"git","url":"git@host:org/r.git","ref":"main"})
        );
        assert_eq!(
            parse("owner/repo@v2").await.unwrap(),
            json!({"source":"github","repo":"owner/repo","ref":"v2"})
        );
        assert_eq!(
            parse("  ").await.unwrap_err().to_string(),
            "Marketplace source is empty"
        );
        assert_eq!(
            parse("nothing").await.unwrap_err().to_string(),
            "Unsupported marketplace source: nothing"
        );
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("m.txt");
        tokio::fs::write(&file, "{}").await.unwrap();
        let text = file.to_string_lossy().into_owned();
        assert!(
            parse_owned(&text)
                .await
                .unwrap_err()
                .to_string()
                .starts_with("Marketplace file must be a .json file")
        );
        let directory = dir.path().to_string_lossy().into_owned();
        assert_eq!(
            parse_owned(&directory).await.unwrap()["source"],
            "directory"
        );
        let missing = format!("{directory}/missing");
        assert!(
            parse_owned(&missing)
                .await
                .unwrap_err()
                .to_string()
                .starts_with("Marketplace source path does not exist")
        );
        assert_eq!(
            path_input("~/m", "/home/u"),
            Some(PathBuf::from("/home/u/m"))
        );
    }

    async fn parse_owned(input: &str) -> Result<Value> {
        parse(input, "/home/u").await
    }
}
