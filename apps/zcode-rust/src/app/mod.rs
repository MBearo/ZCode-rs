mod auxiliary;
mod commands;
mod engine;
mod event_projection;
mod model_config;
mod queries;
mod subscriptions;
pub use crate::contract::{Event, RunEvent};
pub use engine::Engine;
mod input_validation;

mod agent_loop;

mod context;
mod context_projection;
mod create_session;
mod maintenance;
mod queue_control;

mod attachment_upload;
mod attachments;
mod input_attachments;
mod session_close;
mod session_read;
mod session_residency;

mod busy_input;
mod input_admission;
mod run;

mod question_timers;
mod question_tool;
mod questions;

mod todos;

mod goal_commands;
mod goal_events;
mod goal_loop;
mod mcp;
mod session_list;
mod shared_context;
mod skills;
mod subagent_completion;
mod subagent_tools;
mod subagents;

mod history_commands;

mod file_rewind;

mod file_changes;

mod background_events;
