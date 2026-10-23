use super::*;

fn posix() -> PlanFiles {
    let facts = PathFacts {
        home: Some(EnvPath::new("/Users/dev/")),
        style: PathStyle::Posix,
    };
    PlanFiles::new(&facts, &SessionId::new("sess_1")).unwrap()
}

fn windows() -> PlanFiles {
    let facts = PathFacts {
        home: Some(EnvPath::new(r"C:\Users\dev")),
        style: PathStyle::Windows,
    };
    PlanFiles::new(&facts, &SessionId::new("sess_1")).unwrap()
}

#[test]
fn main_thread_and_subagent_paths_live_under_the_plans_directory() {
    let plans = posix();
    assert_eq!(plans.dir(), "/Users/dev/.zcode/plans");
    assert_eq!(
        plans.path(None).as_str(),
        "/Users/dev/.zcode/plans/sess_1.md"
    );
    assert_eq!(
        plans.path(Some(&AgentId::new("a7"))).as_str(),
        "/Users/dev/.zcode/plans/sess_1-agent-a7.md"
    );
    assert_eq!(
        windows().path(None).as_str(),
        r"C:\Users\dev\.zcode\plans\sess_1.md"
    );
}

#[test]
fn no_home_means_no_plan_file() {
    let facts = PathFacts {
        home: None,
        style: PathStyle::Posix,
    };
    assert_eq!(PlanFiles::new(&facts, &SessionId::new("sess_1")), None);
}

#[test]
fn only_this_sessions_plan_files_match() {
    let plans = posix();
    for path in [
        "/Users/dev/.zcode/plans/sess_1.md",
        "/Users/dev/.zcode/plans/sess_1-agent-a7.md",
        "/Users/dev/.zcode/plans/sess_1-agent-.md",
    ] {
        assert!(plans.contains(path), "{path}");
    }
    for path in [
        "/Users/dev/.zcode/plans/sess_2.md",
        "/Users/dev/.zcode/plans/sess_1x.md",
        "/Users/dev/.zcode/plans/sess_1.txt",
        "/Users/dev/.zcode/plans/sess_1-agent-a7.txt",
        "/Users/dev/.zcode/plans/nested/sess_1.md",
        "/Users/dev/.zcode/sess_1.md",
        "/Users/dev/work/.zcode/plans/sess_1.md",
        "/Users/dev/.zcode/plans",
        "sess_1.md",
    ] {
        assert!(!plans.contains(path), "{path}");
    }
}

#[test]
fn windows_paths_match_without_case_or_separator_differences() {
    let plans = windows();
    assert!(plans.contains(r"C:\Users\dev\.zcode\plans\sess_1.md"));
    assert!(plans.contains(r"c:\users\DEV\.zcode\plans\sess_1-agent-a7.md"));
    assert!(plans.contains("C:/Users/dev/.zcode/plans/sess_1.md"));
    assert!(!plans.contains(r"C:\Users\dev\.zcode\plans\sess_2.md"));
}

fn workspace(root: &str, style: PathStyle, session: &str) -> Option<PlanFiles> {
    let facts = PathFacts { home: None, style };
    PlanFiles::in_workspace(&facts, &EnvPath::new(root), &SessionId::new(session))
}

/// Node 的位置（2026-10-18）：`<工作区>/.zcode/plans/plan-<会话>.md`，子代理传子会话 ID，不加 agent 后缀。
#[test]
fn workspace_plan_files_follow_node() {
    let plans = workspace("/Users/dev/work/", PathStyle::Posix, "sess_1").unwrap();
    assert_eq!(plans.dir(), "/Users/dev/work/.zcode/plans");
    assert_eq!(
        plans.path(None).as_str(),
        "/Users/dev/work/.zcode/plans/plan-sess_1.md"
    );
    let child = workspace(
        "/Users/dev/work",
        PathStyle::Posix,
        "sess_subagent_agent_a7",
    )
    .unwrap();
    assert_eq!(
        child.path(Some(&AgentId::new("a7"))).as_str(),
        "/Users/dev/work/.zcode/plans/plan-sess_subagent_agent_a7.md"
    );
    assert_eq!(
        workspace(r"C:\work", PathStyle::Windows, "sess_1")
            .unwrap()
            .path(None)
            .as_str(),
        r"C:\work\.zcode\plans\plan-sess_1.md"
    );
}

/// Node `sanitizePlanFileSessionId`：连续的非法字符换成一个 `-`，去掉首尾的 `-`；为空时没有计划文件。
#[test]
fn session_ids_are_sanitized_like_node() {
    let plans = workspace("/w", PathStyle::Posix, "  a b/c%%d-.e_ ").unwrap();
    assert_eq!(
        plans.path(None).as_str(),
        "/w/.zcode/plans/plan-a-b-c-d-.e_.md"
    );
    let plans = workspace("/w", PathStyle::Posix, "-x--%y-").unwrap();
    assert_eq!(plans.path(None).as_str(), "/w/.zcode/plans/plan-x---y.md");
    assert_eq!(workspace("/w", PathStyle::Posix, " %/ "), None);
}

#[test]
fn only_this_sessions_workspace_plan_file_matches() {
    let plans = workspace("/Users/dev/work", PathStyle::Posix, "sess_1").unwrap();
    assert!(plans.contains("/Users/dev/work/.zcode/plans/plan-sess_1.md"));
    for path in [
        "/Users/dev/work/.zcode/plans/plan-sess_2.md",
        "/Users/dev/work/.zcode/plans/sess_1.md",
        "/Users/dev/work/.zcode/plans/plan-sess_1-agent-a7.md",
        "/Users/dev/work/.zcode/plans/nested/plan-sess_1.md",
        "/Users/dev/.zcode/plans/plan-sess_1.md",
        "/Users/dev/work/.zcode/plans",
    ] {
        assert!(!plans.contains(path), "{path}");
    }
    let windows = workspace(r"C:\Work", PathStyle::Windows, "sess_1").unwrap();
    assert!(windows.contains("c:/work/.ZCODE/plans/PLAN-SESS_1.md"));
}

fn flags(has_exited: bool, needs_exit_attachment: bool) -> PlanModeFlags {
    PlanModeFlags {
        has_exited,
        needs_exit_attachment,
    }
}

/// 改模式：离开 plan 两个标志都置真，进入 plan 只清待发的退出提醒，模式没变不动。
#[test]
fn mode_transitions_update_the_flags() {
    use PermissionMode::{AcceptEdits, Default, Plan};
    let none = PlanModeFlags::default();
    assert_eq!(none.after_transition(Plan, Default), flags(true, true));
    assert_eq!(none.after_transition(Plan, AcceptEdits), flags(true, true));
    assert_eq!(
        flags(true, true).after_transition(Default, Plan),
        flags(true, false)
    );
    assert_eq!(
        flags(true, true).after_transition(Plan, Plan),
        flags(true, true)
    );
    assert_eq!(
        flags(false, true).after_transition(Default, AcceptEdits),
        flags(false, true)
    );
}

/// 提交 reentry / exit 后清对应的标志，别的 attachment 不动。
#[test]
fn committed_attachments_clear_their_flag() {
    let both = flags(true, true);
    assert_eq!(both.after_commit(REENTRY_SOURCE), flags(false, true));
    assert_eq!(both.after_commit(EXIT_SOURCE), flags(true, false));
    assert_eq!(both.after_commit("plan_mode"), both);
}

/// 计划目录与执行环境里别的 `.zcode` 一样用 `PathStyle::join` 拼（修复线 F58，同 Node `path.join`）：普通路径不变；
/// 带 `..` 或 Windows 上写成 `C:/…` 时规范化。以前只去末尾的分隔符再拼。
#[test]
fn plan_dirs_are_joined_like_the_other_dot_dirs() {
    assert_eq!(
        workspace("/Users/x/../dev/work", PathStyle::Posix, "s")
            .unwrap()
            .dir(),
        "/Users/dev/work/.zcode/plans"
    );
    assert_eq!(
        workspace("C:/Users/dev/work", PathStyle::Windows, "s")
            .unwrap()
            .dir(),
        r"C:\Users\dev\work\.zcode\plans"
    );
    let facts = PathFacts {
        home: Some(EnvPath::new("/Users/x/../dev/")),
        style: PathStyle::Posix,
    };
    assert_eq!(
        PlanFiles::new(&facts, &SessionId::new("s")).unwrap().dir(),
        "/Users/dev/.zcode/plans"
    );
}
