//! Errors of plugin management with the classification Node derives from
//! error classes (`UnsupportedMarketplaceSourceError`,
//! `UnsupportedPluginSourceError`, `PluginSourceMaterializationError`,
//! `MarketplaceSourceRepointError`; `source-errors.ts`). Spec
//! rust-m10-4-plugin-sources §8.
use anyhow::anyhow;
use tokio_util::sync::CancellationToken;

/// Node `throwIfPluginOperationAborted`'s message.
pub const CANCELLED: &str = "Plugin operation cancelled";

#[derive(Debug, Clone)]
pub enum Failure {
    /// A recognized but unsupported marketplace or plugin source.
    Unsupported(String),
    /// A source materialization failure carrying its own diagnostic code.
    Source { code: &'static str, message: String },
    /// A declared marketplace conflicting with the Host's known source.
    Repoint(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(message) | Self::Repoint(message) => f.write_str(message),
            Self::Source { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for Failure {}

pub fn unsupported_marketplace(kind: &str) -> anyhow::Error {
    Failure::Unsupported(format!(
        "Marketplace source is recognized but not supported in this runtime: {kind}"
    ))
    .into()
}

pub fn unsupported_plugin(kind: &str) -> anyhow::Error {
    Failure::Unsupported(format!(
        "Plugin source is recognized but not supported in this runtime: {kind}"
    ))
    .into()
}

/// Node `redactPluginSource`: URL credentials never reach diagnostics.
pub fn redact_source(source: &str) -> String {
    let trimmed = crate::js::trim(source);
    if let Ok(mut url) = url::Url::parse(trimmed) {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        return url.to_string();
    }
    let credentials = trimmed.split_once('@').is_some_and(|(user, _)| {
        user.split_once(':').is_some_and(|(name, secret)| {
            !name.is_empty()
                && !secret.is_empty()
                && !user.chars().any(char::is_whitespace)
                && !secret.contains('@')
        })
    });
    if credentials {
        "configured Git source".into()
    } else {
        trimmed.into()
    }
}

/// Node `createGitUnavailableError`.
pub fn git_unavailable(source: &str) -> anyhow::Error {
    Failure::Source {
        code: "plugin_git_unavailable",
        message: format!(
            "System Git is required for plugin source {}, but git is unavailable on this Agent Host. Install Git on the Agent Host, or use a public GitHub HTTPS or verified ZIP source.",
            redact_source(source)
        ),
    }
    .into()
}

pub fn cancelled() -> anyhow::Error {
    anyhow!(CANCELLED)
}

/// Node `throwIfPluginOperationAborted`.
pub fn check(cancel: &CancellationToken) -> anyhow::Result<()> {
    if cancel.is_cancelled() {
        return Err(cancelled());
    }
    Ok(())
}

pub fn classify(error: &anyhow::Error) -> Option<&Failure> {
    error.downcast_ref::<Failure>()
}

/// Node `appendPluginSourceCleanupError`: a cleanup failure is appended to
/// the primary error's message, keeping its classification.
pub fn append_cleanup(error: anyhow::Error, cleanup: Option<String>) -> anyhow::Error {
    let Some(cleanup) = cleanup else {
        return error;
    };
    let suffix = format!("; plugin source cleanup also failed: {cleanup}");
    match classify(&error).cloned() {
        Some(Failure::Unsupported(message)) => Failure::Unsupported(message + &suffix).into(),
        Some(Failure::Repoint(message)) => Failure::Repoint(message + &suffix).into(),
        Some(Failure::Source { code, message }) => Failure::Source {
            code,
            message: message + &suffix,
        }
        .into(),
        None => anyhow!("{error}{suffix}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_are_redacted_and_classified_like_node() {
        assert_eq!(
            redact_source(" https://user:secret@host.test/r.git "),
            "https://host.test/r.git"
        );
        assert_eq!(redact_source("git@host:org/r.git"), "git@host:org/r.git");
        // 能被 URL 解析的输入（含不透明路径）原样返回，与 WHATWG URL 一致。
        assert_eq!(redact_source("user:secret@host/r"), "user:secret@host/r");
        assert_eq!(
            redact_source("1user:secret@host/r"),
            "configured Git source"
        );
        let error = append_cleanup(git_unavailable("x"), Some("busy".into()));
        assert!(matches!(
            classify(&error),
            Some(Failure::Source {
                code: "plugin_git_unavailable",
                ..
            })
        ));
        assert!(
            error
                .to_string()
                .ends_with("; plugin source cleanup also failed: busy")
        );
        let plain = append_cleanup(anyhow!("boom"), None);
        assert_eq!(plain.to_string(), "boom");
    }
}
