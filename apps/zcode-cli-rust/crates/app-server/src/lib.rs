//! App Server frontend: stdio framing, JSON-RPC routing, V4 topic delivery and
//! host bridging over the runtime transport contract. It owns no session facts.
use zcode_cli_core_api as contract;
use zcode_cli_domain as domain;
mod codec;
mod delivery;
mod routes;
mod server;
mod sink;
pub mod stdio;
pub use server::serve;
pub use sink::Sink;
pub use stdio::{finish, start, storage_prepare};
