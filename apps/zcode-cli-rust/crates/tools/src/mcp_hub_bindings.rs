//! Queries over a session's bound MCP tools.
use super::{Binding, Hub};
use serde_json::{Value, json};

impl Hub {
    fn binding<T>(&self, session: &str, name: &str, f: impl FnOnce(&Binding) -> T) -> Option<T> {
        let state = self.state.read().unwrap();
        let bindings = state.bindings.get(session)?;
        bindings.iter().find(|b| b.name == name).map(f)
    }
    /// The annotations Node records for a bound MCP tool.
    pub fn hints(&self, session: &str, name: &str) -> Option<Value> {
        self.binding(
            session,
            name,
            |b| json!({"readOnlyHint": b.read_only, "destructiveHint": b.destructive}),
        )
    }
    pub fn safe(&self, session: &str, name: &str) -> bool {
        self.binding(session, name, |b| b.safe).unwrap_or(false)
    }
}
