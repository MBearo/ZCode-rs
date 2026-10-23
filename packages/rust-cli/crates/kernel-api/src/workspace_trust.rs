//! 工作区信任（docs/specs/rust-cli/workspace-trust.md §2）：信任记录的键。
//!
//! 判定（直接信任、经父目录、home）与键的规范化由入口做；存储端口只按键读写记录
//! （`SessionStore::trusted_workspace` / `trust_workspace`）。

use serde::{Deserialize, Serialize};

/// 信任记录的键：工作区规范化后的绝对路径（workspace-trust.md §2.2），例如 `/Users/dev/work`、
/// `c:/users/dev/work`。只承载规范化的结果，不在这里规范化。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TrustKey(String);

impl TrustKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TrustKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::TrustKey;

    #[test]
    fn the_key_serializes_as_a_plain_string() {
        let key = TrustKey::new("c:/users/dev/work");
        let text = serde_json::to_string(&key).unwrap();
        assert_eq!(text, "\"c:/users/dev/work\"");
        assert_eq!(serde_json::from_str::<TrustKey>(&text).unwrap(), key);
        assert_eq!(key.to_string(), key.as_str());
    }

    #[cfg(feature = "testing")]
    #[tokio::test]
    async fn the_memory_store_returns_the_first_trusted_candidate() {
        use crate::store::SessionStore;
        let store = crate::testing::MemoryStore::default();
        let (work, parent) = (TrustKey::new("/w/a"), TrustKey::new("/w"));
        assert_eq!(
            store
                .trusted_workspace(std::slice::from_ref(&work))
                .await
                .unwrap(),
            None
        );
        store.trust_workspace(&parent).await.unwrap();
        store.trust_workspace(&parent).await.unwrap();
        let found = store
            .trusted_workspace(&[work.clone(), parent.clone()])
            .await
            .unwrap();
        assert_eq!(found, Some(parent.clone()));
        assert_eq!(store.trusted_keys(), vec![parent]);
    }
}
