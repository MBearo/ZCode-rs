//! Diagnostics of failed management operations (Node `marketplace.ts`
//! `toValidationDiagnostic`; `bootstrap/src/plugins.ts`
//! `toMarketplaceInstallDiagnostic`, `toMarketplaceRefreshDiagnostic`).
//! Spec rust-m10-4-plugin-sources §8.
use crate::failure::{Failure, classify};
use crate::manifest::{Diagnostic, Severity};

fn error(code: &'static str, message: String, plugin: Option<&str>) -> Diagnostic {
    let diagnostic = Diagnostic::new(code, Severity::Error, message);
    match plugin {
        Some(id) => diagnostic.plugin(id),
        None => diagnostic,
    }
}

/// Codes derived from the message (Node matches substrings).
fn by_message(message: &str) -> Option<&'static str> {
    if message.contains("Cross-marketplace dependency") {
        Some("plugin_dependency_cross_marketplace")
    } else if message.contains("dependency cycle") {
        Some("plugin_dependency_cycle")
    } else if message.contains("Dependency not found")
        || message.contains("Marketplace not found for dependency")
    {
        Some("plugin_dependency_missing")
    } else {
        None
    }
}

/// Node `toValidationDiagnostic`.
pub fn validation(failure: &anyhow::Error, plugin: Option<&str>) -> Diagnostic {
    let message = failure.to_string();
    let code = match classify(failure) {
        Some(Failure::Unsupported(_)) => "plugin_marketplace_source_unsupported",
        Some(Failure::Source { code, .. }) => code,
        _ => by_message(&message).unwrap_or("plugin_marketplace_invalid"),
    };
    error(code, message, plugin)
}

/// Node `toMarketplaceInstallDiagnostic`.
pub fn install(failure: &anyhow::Error, plugin: &str) -> Diagnostic {
    let message = failure.to_string();
    let code = match classify(failure) {
        Some(Failure::Repoint(_)) => "plugin_marketplace_invalid",
        Some(Failure::Source { code, .. }) => code,
        // Node 按文案 "source is recognized but not supported" 判断；M10.4a 的 zip 占位错误同属此类。
        Some(Failure::Unsupported(_)) => "plugin_marketplace_source_unsupported",
        _ if message.starts_with("Plugin not found:") => "plugin_not_found",
        _ => by_message(&message).unwrap_or(
            if message.contains("source is recognized but not supported") {
                "plugin_marketplace_source_unsupported"
            } else {
                "plugin_marketplace_invalid"
            },
        ),
    };
    error(code, message, Some(plugin))
}

/// Node `toMarketplaceRefreshDiagnostic`.
pub fn refresh(failure: &anyhow::Error, marketplace: &str) -> Diagnostic {
    let code = match classify(failure) {
        Some(Failure::Source { code, .. }) => code,
        _ => "plugin_marketplace_invalid",
    };
    error(code, failure.to_string(), Some(marketplace))
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn failures_map_to_node_codes() {
        let code = |e: anyhow::Error| validation(&e, Some("p@m")).code;
        assert_eq!(
            code(crate::failure::unsupported_marketplace("npm")),
            "plugin_marketplace_source_unsupported"
        );
        assert_eq!(
            code(crate::failure::git_unavailable("u")),
            "plugin_git_unavailable"
        );
        assert_eq!(
            code(anyhow!("Plugin dependency cycle: a -> a")),
            "plugin_dependency_cycle"
        );
        assert_eq!(
            code(anyhow!("Marketplace not found for dependency: x")),
            "plugin_dependency_missing"
        );
        assert_eq!(code(anyhow!("other")), "plugin_marketplace_invalid");
        assert_eq!(
            install(&anyhow!("Plugin not found: a@m"), "a@m").code,
            "plugin_not_found"
        );
        assert_eq!(
            install(&crate::failure::unsupported_plugin("npm"), "a@m").code,
            "plugin_marketplace_source_unsupported"
        );
        assert_eq!(
            install(&crate::failure::Failure::Repoint("r".into()).into(), "a@m").code,
            "plugin_marketplace_invalid"
        );
        assert_eq!(refresh(&anyhow!("x"), "m").plugin_id.as_deref(), Some("m"));
    }
}
