//! Startup options of the engine.
use super::Engine;
use serde_json::Value;
use std::sync::Arc;

impl Engine {
    /// Config file `permission` section (mode fallback, allowed/disallowed tools),
    /// read once at startup like Node's app creation.
    pub fn with_permission_config(mut self, permission: &Value) -> Self {
        self.permissions.config = crate::domain::permission::Config::from_config(permission);
        self.permissions.config_mode = permission["mode"].as_str().map(str::to_owned);
        self
    }
    pub fn with_registry(
        mut self,
        registry: Option<Arc<dyn crate::contract::ModelRegistry>>,
        workspace_path: String,
    ) -> Self {
        self.registry = registry;
        self.workspace_path = workspace_path;
        if let Some(registry) = &self.registry {
            self.config = registry.default_selection();
        }
        self
    }
    /// Node protocol sessions generate their title after the first input
    /// (`titleGeneration: {}` of `createRecord`); `-p` and embedders do not.
    pub fn with_title_generation(mut self, enabled: bool) -> Self {
        self.titles = enabled;
        self
    }
}
