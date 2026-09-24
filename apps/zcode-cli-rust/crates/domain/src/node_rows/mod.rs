//! Rebuilding the V4 conversation of a Node transcript after a restart: the
//! session events Node synthesizes from the stored transcript
//! (`synthesizeEventsFromMessages` and the cold merge), replayed through the
//! product projection. Pure over already decoded rows. Spec
//! rust-m11-node-storage §6.
mod actions;
mod events;
mod facts;
pub mod policy;
mod projection;
mod projection_state;
mod reduce_config;
mod reduce_finish;
mod reduce_goal;
mod reduce_marks;
mod reduce_plan;
mod reduce_stream;
mod reduce_subagents;
mod reduce_tools;
mod reduce_turn;
mod schemas;
mod synth;
mod synth_compact;
mod synth_goals;
mod synth_models;
mod synth_parts;
mod synth_subagents;
mod synth_turn;

pub use events::Event;
pub use projection::{Cold, replay};
pub use synth::{Sources, cold_events};
pub use synth_goals::{GoalEntry, entries as goal_entries};
