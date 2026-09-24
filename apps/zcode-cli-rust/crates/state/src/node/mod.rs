//! The shared Node session database: opening and migrating it, and its
//! repositories with Node's SQL. Spec rust-m11-node-storage.
pub mod acks;
pub mod apply;
pub mod artifacts;
pub mod codecs;
pub mod cold;
pub mod compact;
pub mod entries;
mod fork;
mod fork_bundle;
mod fork_clone;
pub mod full_access;
pub mod input_history;
pub mod inputs;
pub mod listing;
pub mod load;
pub use zcode_cli_domain::js_json as json;
pub mod messages;
pub mod migrations;
pub mod open;
mod open_error;
pub mod resume;
pub mod sessions;
pub mod settings;
pub mod target_row;
pub mod targets;
pub mod todos;

#[cfg(test)]
mod compact_tests;
#[cfg(test)]
mod fixture_tests;
#[cfg(test)]
mod repo_tests;
#[cfg(test)]
#[path = "session_tests.rs"]
mod session_tests;
#[cfg(test)]
mod test_db;
