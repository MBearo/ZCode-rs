//! Official plugin definitions and default marketplaces, generated from the
//! TS source (`scripts/zcode-cli-rust-plugin-fixtures.mjs`).
use serde_json::Value;
use std::sync::LazyLock;

pub const MARKETPLACE: &str = "zcode-plugins-official";
pub const INLINE: &str = "inline";

pub static DATA: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../schema/official.json")).expect("generated JSON")
});

pub fn definitions() -> &'static [Value] {
    DATA["definitions"].as_array().map_or(&[], Vec::as_slice)
}

/// Node `DEFAULT_ENABLED_OFFICIAL_PLUGIN_IDS`.
pub fn enabled_by_default(id: &str) -> bool {
    id.strip_suffix(MARKETPLACE)
        .and_then(|name| name.strip_suffix('@'))
        .is_some_and(|name| {
            definitions()
                .iter()
                .any(|d| d["name"] == name && d["defaultEnabled"] == true)
        })
}

/// Node `resolveOfficialPluginHostMcpServerNames`.
pub fn host_mcp_server_names(id: &str) -> Vec<String> {
    let Some(name) = id
        .strip_suffix(MARKETPLACE)
        .and_then(|n| n.strip_suffix('@'))
    else {
        return vec![];
    };
    definitions()
        .iter()
        .find(|d| d["name"] == name)
        .and_then(|d| d["hostMcpServerNames"].as_array())
        .map(|names| {
            names
                .iter()
                .filter_map(|n| n.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_definitions_are_consistent_with_node() {
        assert_eq!(DATA["marketplace"], MARKETPLACE);
        assert_eq!(DATA["inlineMarketplace"], INLINE);
        assert!(enabled_by_default("skill-creator@zcode-plugins-official"));
        assert!(!enabled_by_default("skill-creator@other"));
        assert_eq!(
            host_mcp_server_names("computer-use@zcode-plugins-official"),
            vec!["node_repl"]
        );
    }
}
