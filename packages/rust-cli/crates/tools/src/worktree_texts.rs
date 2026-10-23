//! EnterWorktree / ExitWorktree 的固定文本（docs/specs/rust-cli/tools-more.md §11），产品名已替换。

/// worktree 名字的总长上限。
pub(crate) const MAX_NAME: usize = 64;

pub(crate) const BOTH_GIVEN: &str = "Provide at most one of `name` or `path`, not both.";

pub(crate) const ALREADY_IN: &str = "Already in a worktree session. Pass `path` to switch into another existing worktree, or use ExitWorktree to leave this one before creating a new worktree.";

pub(crate) const NOT_A_REPO: &str = "Cannot create a worktree: not in a git repository and no WorktreeCreate hooks are configured. Configure WorktreeCreate/WorktreeRemove hooks in settings.json to use worktree isolation with other VCS systems.";

pub(crate) const RESUMED_NOTE: &str = " A worktree with this name already existed and was resumed as-is \u{2014} it may carry an earlier session\u{2019}s commits. Pass a different name if you wanted a fresh worktree.";

pub(crate) const NO_SESSION: &str = "No-op: there is no active EnterWorktree session to exit. This tool only operates on worktrees created by EnterWorktree in the current session \u{2014} it will not touch worktrees created manually or in a previous session. No filesystem changes were made.";

pub(crate) fn invalid_name(name: &str, reason: &str) -> String {
    format!("Invalid worktree name \"{name}\": {reason}")
}

pub(crate) fn too_long(length: usize) -> String {
    format!("Invalid worktree name: must be {MAX_NAME} characters or fewer (got {length})")
}

pub(crate) fn ask_enter(path: &str) -> String {
    format!(
        "Enter the worktree at \"{path}\"? This moves the session's working directory and write access there, and loads project configuration (AGENTS.md, settings) from that location."
    )
}

pub(crate) fn not_registered(path: &str, root: &str) -> String {
    format!(
        "Cannot enter worktree: {path} is not a registered worktree of {root}. Run 'git -C {root} worktree list' to see registered worktrees."
    )
}

fn on_branch(branch: Option<&str>) -> String {
    branch.map_or_else(String::new, |branch| format!(" on branch {branch}"))
}

/// EnterWorktree 的结果（`Created` / `Resumed` / `Entered`）。
pub(crate) fn entered(verb: &str, path: &str, branch: Option<&str>, note: &str) -> String {
    format!(
        "{verb} worktree at {path}{}.{note} The session is now working in the worktree. Use ExitWorktree to leave mid-session, or exit the session to be prompted.",
        on_branch(branch)
    )
}

pub(crate) fn not_owner(path: &str, original: &str) -> String {
    format!(
        "This session is not the owner of the worktree at {path} \u{2014} it either entered a pre-existing worktree via EnterWorktree({{path}}) or resumed into a checkout whose liveness lock another running ZCode session still holds \u{2014} so this tool will not remove it. Use action: \"keep\" to return to {original}. If no other session is using it, remove it yourself with `git worktree remove`; while a live session's lock is present, git will refuse and name the owner."
    )
}

pub(crate) fn unverified(path: &str) -> String {
    format!(
        "Could not verify worktree state at {path}. Refusing to remove without explicit confirmation. Re-invoke with discard_changes: true to proceed \u{2014} or use action: \"keep\" to preserve the worktree."
    )
}

fn count(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

pub(crate) fn has_work(files: u64, commits: u64, branch: Option<&str>) -> String {
    let mut parts = Vec::new();
    if files > 0 {
        parts.push(format!(
            "{} uncommitted {}",
            files,
            if files == 1 { "file" } else { "files" }
        ));
    }
    if commits > 0 {
        let on = branch.unwrap_or("the worktree branch");
        parts.push(format!("{} on {on}", count(commits, "commit", "commits")));
    }
    format!(
        "Worktree has {}. Removing will discard this work permanently. Confirm with the user, then re-invoke with discard_changes: true \u{2014} or use action: \"keep\" to preserve the worktree.",
        parts.join(" and ")
    )
}

/// 回到原目录（原目录还在时）。
pub(crate) fn back_in(original: &str) -> String {
    format!("Session is now back in {original}.")
}

pub(crate) fn kept(path: &str, branch: Option<&str>, original: &str) -> String {
    format!(
        "Exited worktree. Your work is preserved at {path}{}. {}",
        on_branch(branch),
        back_in(original)
    )
}

pub(crate) fn removed(path: &str, files: u64, commits: u64, original: &str) -> String {
    let mut parts = Vec::new();
    if commits > 0 {
        parts.push(count(commits, "commit", "commits"));
    }
    if files > 0 {
        parts.push(format!(
            "{files} uncommitted {}",
            if files == 1 { "file" } else { "files" }
        ));
    }
    let discarded = if parts.is_empty() {
        String::new()
    } else {
        format!(" Discarded {}.", parts.join(" and "))
    };
    format!(
        "Exited and removed worktree at {path}.{discarded} {}",
        back_in(original)
    )
}

pub(crate) fn not_removed(path: &str, original: &str) -> String {
    format!(
        "Exited worktree but could not remove it \u{2014} kept at {path}. {}",
        back_in(original)
    )
}
