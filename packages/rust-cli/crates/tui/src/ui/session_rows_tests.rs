//! 两页共用的会话行（F29）：标题、搜索、时间与路径的写法。
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::panels::ProjectSession;
use rust_cli_kernel_api::store::SessionRecord;

use super::*;

fn session(title: &str, prompt: Option<&str>) -> ProjectSession {
    ProjectSession {
        record: SessionRecord {
            id: SessionId::new("sess_Abc"),
            title: title.to_owned(),
            root: EnvPath::new("/Users/dev/Work/App"),
            created_at_ms: 0,
            updated_at_ms: 0,
            first_prompt: prompt.map(str::to_owned),
        },
        activity: None,
        last_reply: None,
        branch: Some("Feature/X".to_owned()),
    }
}

#[test]
fn the_label_is_the_name_then_the_first_prompt_line() {
    assert_eq!(
        SessionRow::new(&session("  Fix it \nmore", None), None).label(),
        "Fix it"
    );
    assert_eq!(
        SessionRow::new(&session("", Some("\n  first line\nsecond")), None).label(),
        "first line"
    );
    assert_eq!(SessionRow::new(&session("", None), None).label(), "");
    assert!(SessionRow::new(&session("", None), Some("sess_Abc")).current);
}

#[test]
fn search_is_a_case_insensitive_substring_match() {
    let row = SessionRow::new(&session("Fix Login", Some("Please HELP")), None);
    for query in ["login", "help", "work/app", ""] {
        assert!(row.matches(query, false), "{query}");
    }
    // 会话 ID 与分支只在 `/resume` 页比。
    assert!(!row.matches("abc", false));
    assert!(row.matches("abc", true));
    assert!(row.matches("feature/x", true));
    assert!(!row.matches("nothing", true));
}

#[test]
fn relative_times_short_and_long() {
    let now = 10 * 86_400_000;
    let at = |seconds: u64| now - seconds * 1000;
    let cases = [
        (0, "now"),
        (42, "42s ago"),
        (35 * 60, "35m ago"),
        (2 * 3600, "2h ago"),
        (86_400, "1d ago"),
    ];
    for (seconds, text) in cases {
        assert_eq!(relative(Lang::En, now, at(seconds)), text);
    }
    assert_eq!(relative(Lang::Zh, now, at(35 * 60)), "35 分钟前");
    assert_eq!(relative_long(Lang::En, now, at(60)), "1 minute ago");
    assert_eq!(relative_long(Lang::En, now, at(5 * 60)), "5 minutes ago");
    assert_eq!(timestamp(1_760_000_000_000, 0), "2025-10-09 08:53:20");
    assert_eq!(timestamp(1_760_000_000_000, 480), "2025-10-09 16:53:20");
}

#[test]
fn long_paths_are_cut_in_the_middle() {
    assert_eq!(center_truncate("~/work/app", 20), "~/work/app");
    let cut = center_truncate("~/work/a-very-long-project-name", 12);
    assert_eq!(cut.width(), 12);
    assert!(
        cut.starts_with("~/wor") && cut.ends_with("name") && cut.contains('…'),
        "{cut}"
    );
}
