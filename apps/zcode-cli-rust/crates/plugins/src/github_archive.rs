//! Public GitHub repositories fetched as a zipball before falling back to
//! system Git (Node `github-archive-source.ts`, `source-errors.ts`
//! `createArchiveFetchError`). Spec rust-m10-4-plugin-sources §9.
use crate::failure::Failure;
use crate::plugin_source::Resolved;
use crate::ports::Ports;
use crate::zip_source::{DownloadFailure, HttpZip};
use anyhow::Result;
use std::path::Path;
use std::sync::LazyLock;

const FALLBACK_STATUSES: [u16; 3] = [401, 403, 404];
const USER_AGENT: &str = "ZCode-Plugin-Installer";

/// Node `GitHubArchiveRequiresGitError`.
#[derive(Debug)]
pub struct RequiresGit(pub String);

impl std::fmt::Display for RequiresGit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GitHub Archive requires system Git fallback: {}", self.0)
    }
}

impl std::error::Error for RequiresGit {}

fn segment(text: &str) -> bool {
    !text.is_empty()
        && text != "."
        && text != ".."
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
}

/// Node `parsePublicGitHubRepositoryUrl`: `(owner, repo)`.
fn public_repository(value: &str) -> Option<(String, String)> {
    let url = url::Url::parse(value).ok()?;
    let host = url.host_str()?.to_lowercase();
    if url.scheme() != "https"
        || !(host == "github.com" || host == "www.github.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some_and(|q| !q.is_empty())
        || url.fragment().is_some_and(|f| !f.is_empty())
    {
        return None;
    }
    let segments: Vec<&str> = url.path().split('/').filter(|s| !s.is_empty()).collect();
    let [owner, repo] = segments.as_slice() else {
        return None;
    };
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    (segment(owner) && segment(repo)).then(|| ((*owner).to_owned(), repo.to_owned()))
}

/// `encodeURIComponent`.
fn encode_component(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Whether `.gitattributes` in `dir` (and below when `recursive`) routes files through LFS.
async fn declares_lfs(dir: &Path, recursive: bool) -> Result<bool> {
    let attributes = dir.join(".gitattributes");
    if crate::fsx::is_file(&attributes).await {
        let text = tokio::fs::read_to_string(&attributes).await?;
        if text.split_whitespace().any(|token| token == "filter=lfs") {
            return Ok(true);
        }
    }
    if !recursive {
        return Ok(false);
    }
    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_type().await?.is_dir() && Box::pin(declares_lfs(&entry.path(), true)).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Node `detectRequiredGitSemantics`: submodules and LFS need real Git.
async fn required_git(repository: &Path, selected: &Path) -> Result<Option<&'static str>> {
    if crate::fsx::is_file(&repository.join(".gitmodules")).await {
        return Ok(Some("repository declares Git submodules"));
    }
    if declares_lfs(repository, selected == repository).await? {
        return Ok(Some("repository declares Git LFS filters"));
    }
    if selected != repository {
        // Git attributes 从仓库根逐级继承到目标目录，父目录的 LFS 规则同样生效。
        let mut current = selected.parent();
        while let Some(dir) = current.filter(|d| *d != repository) {
            if declares_lfs(dir, false).await? {
                return Ok(Some("selected plugin path inherits Git LFS filters"));
            }
            current = dir.parent();
        }
        if declares_lfs(selected, true).await? {
            return Ok(Some("selected plugin path declares Git LFS filters"));
        }
    }
    Ok(None)
}

/// Node `resolveGitHubArchiveSource`.
pub async fn resolve(
    ports: &Ports<'_>,
    url: &str,
    pin: Option<&str>,
    path: Option<&str>,
) -> Result<Resolved> {
    let Some((owner, repo)) = public_repository(url) else {
        return Err(RequiresGit(format!(
            "source is not a public GitHub HTTPS repository: {url}"
        ))
        .into());
    };
    let pin = pin
        .map(crate::js::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or("HEAD");
    let archive = format!(
        "https://api.github.com/repos/{owner}/{repo}/zipball/{}",
        encode_component(pin)
    );
    let input = HttpZip {
        url: &archive,
        headers: vec![
            ("Accept".into(), "application/vnd.github+json".into()),
            ("User-Agent".into(), USER_AGENT.into()),
        ],
        path: None,
        sha256: None,
        strip_root: Some(true),
        require_single_root: true,
    };
    let resolved = crate::zip_source::resolve_http(ports, input).await?;
    let result = async {
        let mut selected = resolved.path.clone();
        if let Some(path) = path.filter(|p| !p.is_empty()) {
            match crate::fsx::resolve_inside(&resolved.path, path) {
                Some(subdir) if crate::fsx::is_dir(&subdir).await => selected = subdir,
                _ => anyhow::bail!("Plugin source subdirectory does not exist: {path}"),
            }
        }
        if let Some(reason) = required_git(&resolved.path, &selected).await? {
            return Err(RequiresGit(reason.into()).into());
        }
        Ok(selected)
    }
    .await;
    match result {
        Ok(path) => Ok(Resolved {
            path,
            cleanup: resolved.cleanup,
        }),
        Err(error) => {
            let cleanup = crate::store::cleanup(resolved.cleanup.as_deref()).await;
            Err(crate::failure::append_cleanup(error, cleanup))
        }
    }
}

/// Node `shouldFallbackGitHubArchiveToGit`.
pub fn should_fallback(error: &anyhow::Error) -> bool {
    if error.is::<RequiresGit>() {
        return true;
    }
    if error
        .downcast_ref::<DownloadFailure>()
        .is_some_and(|f| FALLBACK_STATUSES.contains(&f.status))
    {
        return true;
    }
    let message = error.to_string().to_lowercase();
    message.contains("plugin zip entry symlinks are not supported")
        || message.contains("unsupported plugin zip entry type")
}

static URL_TEXT: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r#"(?i)(?-u:\b)[a-z][a-z\d+.-]*://[^\s"'<>()\[\]{}]+"#)
        .expect("valid pattern")
});
static CREDENTIAL_TEXT: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?-u:\b)[^\s:@]+:[^\s@]+@\S+").expect("valid pattern"));

/// Node `redactPluginDiagnosticText`.
fn redact_text(text: &str) -> String {
    let urls = URL_TEXT.replace_all(text, |m: &regex::Captures<'_>| {
        crate::failure::redact_source(&m[0])
    });
    CREDENTIAL_TEXT
        .replace_all(&urls, "configured Git source")
        .into_owned()
}

/// Node `createArchiveFetchError`.
pub fn fetch_error(source: &str, cause: &anyhow::Error) -> anyhow::Error {
    Failure::Source {
        code: "plugin_archive_fetch_failed",
        message: format!(
            "Failed to materialize public GitHub plugin source archive {}: {}",
            crate::failure::redact_source(source),
            redact_text(&cause.to_string())
        ),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_repositories_and_redaction_follow_node() {
        assert_eq!(
            public_repository("https://github.com/o/r.git"),
            Some(("o".into(), "r".into()))
        );
        assert_eq!(public_repository("https://u:p@github.com/o/r"), None);
        assert_eq!(public_repository("https://github.com/o/r/tree/x"), None);
        assert_eq!(public_repository("http://github.com/o/r"), None);
        assert_eq!(encode_component("v1/rc 1"), "v1%2Frc%201");
        let cause = anyhow::anyhow!("GET https://u:p@host.test/x failed for a:b@c");
        let error = fetch_error("https://u:p@github.com/o/r.git", &cause);
        assert_eq!(
            error.to_string(),
            "Failed to materialize public GitHub plugin source archive https://github.com/o/r.git: GET https://host.test/x failed for configured Git source"
        );
        assert!(should_fallback(&RequiresGit("x".into()).into()));
        let missing: anyhow::Error = DownloadFailure {
            status: 404,
            message: "m".into(),
        }
        .into();
        assert!(should_fallback(&missing));
        assert!(!should_fallback(&anyhow::anyhow!("fetch failed")));
    }
}
