//! Plugin discovery and management shared by the runtime (skills, MCP,
//! hooks) and the `plugins/*` methods. Spec rust-m10-plugins.
pub(crate) use zcode_cli_domain::js_string as js;

pub mod atomic;
pub mod catalog;
pub mod commands;
pub mod components;
pub mod discovery;
pub mod frontmatter;
pub mod fsx;
pub mod hook_schema;
pub mod hooks;
pub mod list;
pub mod loaded;
pub mod manifest;
pub mod market;
pub mod mcp;
pub mod official;
pub mod overview;
pub mod records;
pub mod version;

pub use discovery::{Outcome, Plugin, Request, SkillRoot, discover};
pub use manifest::{Diagnostic, Severity};
