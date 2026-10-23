use super::*;

/// `--help`（不是终端，宽度 80）里的几项原文：说明一栏折行、名字太长时另起一行。
const EXPECTED_FALLBACK: &str =
    "  --fallback-model <model>              Enable automatic fallback to specified
                                        model(s) when the default model is
                                        overloaded or not available. Accepts a
                                        comma-separated list to try each in
                                        order. Re-tries the primary at the start
                                        of each user turn.";

const EXPECTED_ALLOWED: &str = "  --allowedTools, --allowed-tools <tools...>
      Comma or space-separated list of tool names to allow (e.g. \"Bash(git *)
      Edit\")";

const EXPECTED_MODE: &str =
    "  --permission-mode <mode>              Permission mode to use for the session
                                        (choices: \"acceptEdits\", \"auto\",
                                        \"bypassPermissions\", \"manual\",
                                        \"dontAsk\", \"plan\")";

fn find(term: &str) -> &'static HelpItem {
    OPTIONS.iter().find(|item| item.term == term).unwrap()
}

#[test]
fn items_wrap_at_eighty_columns() {
    // 本工程不列 `--fallback-model`（cli-args.md D4），只用它的原文检查折行。
    let fallback = HelpItem {
        term: "--fallback-model <model>",
        description: "Enable automatic fallback to specified model(s) when the default model is overloaded or not available. Accepts a comma-separated list to try each in order. Re-tries the primary at the start of each user turn.",
    };
    assert_eq!(format_item(&fallback, 36, 80), EXPECTED_FALLBACK);
    assert_eq!(
        format_item(find("--allowedTools, --allowed-tools <tools...>"), 36, 80),
        EXPECTED_ALLOWED
    );
    // 工作线 F63（cli-args.md §3.2）：choices 在原有的六个之后多一个 `"yolo"`，折行规则不变，只是最后一行长了。
    assert_eq!(
        format_item(find("--permission-mode <mode>"), 36, 80),
        EXPECTED_MODE.replace("\"plan\")", "\"plan\", \"yolo\")")
    );
}

/// 说明一栏不足 30 列时每项都另起一行。
#[test]
fn a_narrow_terminal_stacks_every_description() {
    let narrow = format_item(find("--fork-session"), 36, 60);
    assert_eq!(
        narrow,
        "  --fork-session\n      When resuming, create a new session ID instead of\n      reusing the original (use with --resume or --continue)"
    );
}

#[test]
fn wrapping_keeps_words_and_drops_the_leading_space_of_a_new_line() {
    assert_eq!(wrap("aaa bbb ccc", 7), "aaa bbb\nccc");
    // 一个词比宽度还长时单独占一行，不截断。
    assert_eq!(wrap("a verylongword b", 4), "a\nverylongword\nb");
    assert_eq!(wrap("one\n\ntwo", 10), "one\n\ntwo");
}

#[test]
fn the_page_has_the_sections_in_order_and_ends_with_one_newline() {
    let page = render(DEFAULT_WIDTH);
    assert!(page.starts_with(
        "Usage: zcode [options] [command] [prompt]\n\nZCode - starts an interactive session by default, use -p/--print for\nnon-interactive output\n\nArguments:\n  prompt                                Your prompt\n\nOptions:\n"
    ));
    let options = page.find("\nOptions:\n").unwrap();
    let commands = page.find("\nCommands:\n").unwrap();
    assert!(options < commands);
    // 命令按名字排，`update|upgrade`（工作线 F48，update.md §9）排在最后：名字一栏固定，说明是 Codex 的。
    assert!(
        page.contains(
            "  logout                                Log out from Coding Plan accounts\n"
        )
    );
    assert!(
        page.ends_with(
            "  update|upgrade                        Update ZCode to the latest version\n"
        )
    );
    assert!(!page.ends_with("\n\n"));
}

/// 选项按 commander 的 `compareOptions` 排（长名字去掉 `--`；`--allowedTools, --allowed-tools` 按 `allowed-tools`）。
#[test]
fn options_are_sorted_by_long_name() {
    let page = render(DEFAULT_WIDTH);
    let order: Vec<usize> = [
        "  --add-dir",
        "  --allowedTools",
        "  --append-system-prompt",
        "  -c, --continue",
        "  --disallowedTools",
        "  --effort",
        "  --fork-session",
        "  -h, --help",
        "  --mcp-config",
        "  --model",
        "  --permission-mode",
        "  --plugin-dir",
        "  -p, --print",
        "  -r, --resume",
        "  --session-id",
        "  --settings",
        "  --strict-mcp-config",
        "  --system-prompt",
        "  -v, --version",
    ]
    .iter()
    .map(|term| {
        page.find(&format!("\n{term}"))
            .unwrap_or_else(|| panic!("{term}"))
    })
    .collect();
    let mut sorted = order.clone();
    sorted.sort_unstable();
    assert_eq!(order, sorted);
    // 工作线 F63 删掉了这个参数（cli-args.md §5.3），帮助里没有它，未知选项的建议也不会给它。
    assert!(!page.contains("--dangerously-skip-permissions"), "{page}");
    assert!(!visible_longs().contains(&"--dangerously-skip-permissions"));
}

#[test]
fn flags_split_like_commander() {
    assert_eq!(
        split_flags("--allowedTools, --allowed-tools <tools...>"),
        (Some("--allowedTools"), Some("--allowed-tools"))
    );
    assert_eq!(split_flags("-p, --print"), (Some("-p"), Some("--print")));
    assert_eq!(
        split_flags("--add-dir <directories...>"),
        (None, Some("--add-dir"))
    );
    assert_eq!(split_flags("-x"), (Some("-x"), None));
    assert!(visible_longs().contains(&"--allowed-tools"));
    assert!(!visible_longs().contains(&"--allowedTools"));
}

/// `--effort` 的说明照原文，档位的顺序与警告相同（cli-args.md §1.4）。
#[test]
fn the_effort_option_lists_the_levels() {
    let effort = find("--effort <level>");
    assert_eq!(
        effort.description,
        format!(
            "Effort level for the current session ({})",
            crate::effort::LEVELS.join(", ")
        )
    );
}
