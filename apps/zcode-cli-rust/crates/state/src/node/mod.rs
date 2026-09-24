//! The shared Node session database: opening and migrating it, and its
//! repositories with Node's SQL. Spec rust-m11-node-storage.
pub mod codecs;
pub mod entries;
pub mod input_history;
pub mod inputs;
pub mod json;
pub mod messages;
pub mod migrations;
pub mod open;
mod open_error;
pub mod sessions;
pub mod settings;
pub mod targets;
pub mod todos;

#[cfg(test)]
mod fixture_tests;
#[cfg(test)]
mod repo_tests;
