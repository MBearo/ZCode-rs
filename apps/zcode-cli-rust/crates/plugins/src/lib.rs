//! Plugin discovery and management shared by the runtime (skills, MCP,
//! hooks) and the `plugins/*` methods. Spec rust-m10-plugins.
pub(crate) use zcode_cli_domain::js_string as js;

pub mod activation;
pub mod atomic;
pub mod atomic_write;
pub mod catalog;
pub mod closure;
pub mod commands;
pub mod components;
pub mod config_file;
pub mod describe;
pub mod diagnose;
pub mod discovery;
pub mod entry_manifest;
pub mod failure;
pub mod frontmatter;
pub mod fsx;
pub mod git;
pub mod hook_schema;
pub mod hooks;
pub mod http;
pub mod install;
pub mod install_admin;
pub mod known;
pub mod list;
pub mod loaded;
pub mod manifest;
pub mod market;
pub mod market_admin;
pub mod marketplace_ops;
pub mod marketplace_source;
pub mod mcp;
pub mod mutations;
pub mod official;
pub mod official_partition;
pub mod overview;
pub mod plugin_source;
pub mod ports;
pub mod records;
pub mod source_input;
pub mod store;
pub mod validate;
pub mod version;

pub use discovery::{Outcome, Plugin, Request, SkillRoot, discover};
pub use manifest::{Diagnostic, Severity};
