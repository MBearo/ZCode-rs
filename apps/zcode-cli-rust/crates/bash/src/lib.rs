//! Bash permission semantics of the Node CLI (`tool/handlers/bash-*.ts`):
//! read-only classification, "always allow" suggestions and rule matching.
//!
//! Pure: the git runtime context (a filesystem check) is decided by the caller
//! and passed in, only when [`invokes_git`] says the command needs it.
mod argv;
mod callbacks;
mod callbacks_net;
mod flags;
mod git;
mod git_callbacks;
mod js;
mod printf;
mod registry;
mod rules;
mod suggest;
mod tables;
#[cfg(test)]
mod tests;

pub use git::invokes_git;
pub use rules::BashRules;
pub use zcode_cli_bash_parse::{Analysis, Invocation, analyze, is_permission_safe};

/// Node `isRuntimeReadOnlyBashCommand` for an analysed command.
pub fn is_read_only_analysis(analysis: &Analysis, git_unsafe: bool) -> bool {
    if !is_permission_safe(analysis) || analysis.commands.is_empty() {
        return false;
    }
    if git::git_with_directory_change(&analysis.commands) {
        return false;
    }
    if git_unsafe && invokes_git(&analysis.commands) {
        return false;
    }
    analysis
        .commands
        .iter()
        .all(|part| !argv::has_known_write_option(part) && argv::evaluate(part) == Some(true))
}

/// Node `isRuntimeReadOnlyBashCommand`.
pub fn is_read_only(command: &str, git_unsafe: bool) -> bool {
    is_read_only_analysis(&analyze(command), git_unsafe)
}
