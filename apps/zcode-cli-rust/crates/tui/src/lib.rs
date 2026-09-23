//! Terminal UI frontend (reserved).
//!
//! The TUI will connect to the runtime through the same transport contract as
//! the App Server (`zcode_cli_core_api::{ClientMsg, ServerMsg}`) and must never
//! own session, queue, model or tool facts. Until it is implemented, the
//! `tui` subcommand exits with [`UNAVAILABLE_EXIT_CODE`].

/// Exit code of the reserved `tui` entry point.
pub const UNAVAILABLE_EXIT_CODE: i32 = 2;
