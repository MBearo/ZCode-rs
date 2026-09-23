mod agent_profiles;
pub mod config;
mod context_git;
pub mod context_source;
mod legacy_storage;
mod model_failure;
mod model_media;
mod model_policy;
mod model_stream;
pub mod provider;
pub mod registry;
mod sse;
pub mod stdio;
pub mod storage;
pub mod tools;

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub struct SystemClock;
impl crate::contract::RuntimeClock for SystemClock {
    fn now(&self) -> u64 {
        now()
    }
    fn id(&self) -> String {
        id()
    }
}

mod tool_files;
mod tool_search;
mod tool_shell;

mod tool_process;

mod anthropic_stream;
pub mod model_protocol;
mod responses_stream;

mod legacy_attachments;
mod registry_rules;

mod registry_config;

pub mod legacy_paths;

mod input_attachments;
mod legacy_projection;
mod request_attachments;

mod legacy_attempt;
mod legacy_sessions;

#[cfg(unix)]
mod process_tree;

mod legacy_shared_context;
mod legacy_todos;

mod storage_listing;

mod storage_read;

mod extension_config;
mod extension_plugins;
mod mcp_config;
mod mcp_connection;
mod mcp_hub;
mod mcp_sse;
mod storage_index;
mod storage_ports;
mod tool_skills;

mod checkpoint_blobs;
mod file_checkpoints;
mod file_rewind;

mod storage_history;

mod file_changes;
