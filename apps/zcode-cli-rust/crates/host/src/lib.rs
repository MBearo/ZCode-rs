use zcode_cli_core_api as contract;
use zcode_cli_domain as domain;
use std::path::Path;
mod context_git;
pub mod context_source;
pub mod legacy_paths;
mod question_timing;
pub use question_timing::question_timing;
pub use context_source::WorkspaceContext;
pub fn now() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64 }
pub fn id() -> String { uuid::Uuid::new_v4().to_string() }
pub fn workspace_identity(identity: Option<&str>, workspace_path: &Path) -> String {
    identity
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| workspace_path.to_string_lossy().into_owned())
}
pub struct SystemClock;
impl contract::RuntimeClock for SystemClock { fn now(&self) -> u64 { now() } fn id(&self) -> String { id() } }

#[cfg(test)]
mod tests {
    use super::workspace_identity;
    use std::path::Path;

    #[test]
    fn identity_prefers_trimmed_remote_key_and_falls_back_to_path() {
        assert_eq!(workspace_identity(Some("  remote-1  "), Path::new("/repo")), "remote-1");
        assert_eq!(workspace_identity(Some("  "), Path::new("/repo")), "/repo");
        assert_eq!(workspace_identity(None, Path::new("/repo")), "/repo");
    }
}
