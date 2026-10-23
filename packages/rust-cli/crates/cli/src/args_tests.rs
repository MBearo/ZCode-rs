use super::*;

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn mode(values: &[&str]) -> Mode {
    parse(&args(values)).unwrap().mode
}

#[test]
fn no_arguments_open_the_terminal_interface() {
    assert_eq!(mode(&[]), Mode::Tui);
}

#[test]
fn version_and_print_flags() {
    assert_eq!(mode(&["--version"]), Mode::Version);
    assert_eq!(mode(&["-V"]), Mode::Version);
    // `-v`（docs/specs/rust-cli/version.md §5）。
    assert_eq!(mode(&["-v"]), Mode::Version);
    // `-v` / `--version` 是 commander 的选项，与别的参数同用也输出版本；`-V` 只在单独使用时认（version.md §5）。
    assert_eq!(mode(&["-p", "hi", "-v"]), Mode::Version);
    assert_eq!(mode(&["--model", "m", "--version"]), Mode::Version);
    assert_eq!(
        parse(&args(&["-p", "hi", "-V"])).unwrap_err(),
        "error: unknown option '-V'"
    );
    let invocation = parse(&args(&["-p", "hello"])).unwrap();
    assert_eq!(invocation.mode, Mode::Print);
    assert_eq!(invocation.prompt.as_deref(), Some("hello"));
    let invocation = parse(&args(&["--model", "m1", "-p", "hi"])).unwrap();
    assert_eq!(invocation.model.as_deref(), Some("m1"));
    // 没有 prompt 时由入口从标准输入读（cli-args.md §2.3）。
    let invocation = parse(&args(&["-p"])).unwrap();
    assert_eq!((invocation.mode, invocation.prompt), (Mode::Print, None));
}

/// 未知选项与缺值的报错照 commander 原文，退出码 1（cli-args.md §2.2）。修复前是自拟的
/// `zcode: unsupported arguments: …`、退出码 2。
#[test]
fn unknown_arguments_are_rejected_with_the_commander_text() {
    assert_eq!(
        parse(&args(&["--nope"])).unwrap_err(),
        "error: unknown option '--nope'"
    );
    assert_eq!(
        parse(&args(&["-p", "hi", "--modle", "m"])).unwrap_err(),
        "error: unknown option '--modle'\n(Did you mean --model?)"
    );
    assert_eq!(
        parse(&args(&["-p", "hi", "--model"])).unwrap_err(),
        "error: option '--model <model>' argument missing"
    );
    assert_eq!(
        parse(&args(&["-p", "hi", "--max-turns", "abc"])).unwrap_err(),
        "error: option '--max-turns <turns>' argument 'abc' is invalid. must be a number"
    );
    // 工作线 F63：choices 在原有的六个之后多一个本工程的 `yolo`；Node 的 `guarded` 仍不认。
    assert_eq!(
        parse(&args(&["-p", "hi", "--permission-mode", "guarded"])).unwrap_err(),
        "error: option '--permission-mode <mode>' argument 'guarded' is invalid. Allowed choices are acceptEdits, auto, bypassPermissions, manual, dontAsk, plan, yolo."
    );
}

/// 工作线 F63（cli-args.md §5.3，用户决定 2026-10-22）：`--permission-mode yolo` 等于 bypassPermissions；
/// `--dangerously-skip-permissions` 删掉，按 commander 的未知选项报错（没有建议行）。
#[test]
fn yolo_is_bypass_permissions_and_the_old_flag_is_unknown() {
    for line in [
        &["-p", "hi", "--permission-mode", "yolo"][..],
        &["-p", "hi", "--permission-mode=yolo"][..],
        &["--permission-mode", "yolo"][..],
    ] {
        let parsed = parse(&args(line)).unwrap();
        assert_eq!(
            parsed.permission_mode,
            Some(PermissionMode::BypassPermissions),
            "{line:?}"
        );
    }
    assert_eq!(
        parse(&args(&["-p", "hi", "--permission-mode", "yolo"])).unwrap(),
        parse(&args(&[
            "-p",
            "hi",
            "--permission-mode",
            "bypassPermissions"
        ]))
        .unwrap()
    );
    for line in [
        &["-p", "hi", "--dangerously-skip-permissions"][..],
        &["--dangerously-skip-permissions"][..],
        &[
            "--dangerously-skip-permissions",
            "-p",
            "hi",
            "--permission-mode",
            "yolo",
        ][..],
    ] {
        assert_eq!(
            parse(&args(line)).unwrap_err(),
            "error: unknown option '--dangerously-skip-permissions'",
            "{line:?}"
        );
    }
}

/// `-h` / `--help` 在任何位置都输出帮助，先于未知选项的报错（commander `_outputHelpIfRequested`）。
#[test]
fn help_wins_over_unknown_options() {
    assert_eq!(mode(&["-h"]), Mode::Help);
    assert_eq!(mode(&["--help"]), Mode::Help);
    assert_eq!(mode(&["-p", "hi", "--nope", "-h"]), Mode::Help);
    // 作为选项的值时不是帮助。
    assert_eq!(
        parse(&args(&["--model", "-h"])).unwrap().model.as_deref(),
        Some("-h")
    );
}

/// 跑分框架的命令行（cli-args.md §1.2）：可变参数选项、`=` 写法、`--` 之后的 prompt。
#[test]
fn benchmark_command_lines_parse() {
    // Terminal-Bench 1：`--allowedTools` 在最后，后面的工具名都归它。
    let tb = parse(&args(&[
        "-p",
        "fix the bug",
        "--allowedTools",
        "Bash",
        "Edit",
        "Write",
        "Read",
    ]))
    .unwrap();
    assert_eq!(tb.prompt.as_deref(), Some("fix the bug"));
    assert_eq!(tb.allowed, ["Bash", "Edit", "Write", "Read"]);
    // Harbor：`--permission-mode=bypassPermissions --print`，prompt 从标准输入来。
    let harbor = parse(&args(&["--permission-mode=bypassPermissions", "--print"])).unwrap();
    assert_eq!(harbor.mode, Mode::Print);
    assert_eq!(
        harbor.permission_mode,
        Some(PermissionMode::BypassPermissions)
    );
    assert_eq!(harbor.prompt, None);
    // Inspect-SWE：`--session-id <uuid> --dangerously-skip-permissions … -- <prompt>`。工作线 F63 删掉了旧参数：原样的
    // 写法报未知选项，适配器换成 `--permission-mode bypassPermissions`（cli-args.md §1.2）。
    let inspect = |permission: &[&'static str]| {
        let mut line = vec!["--session-id", "00000000-0000-4000-8000-000000000001"];
        line.extend_from_slice(permission);
        line.extend([
            "--model",
            "m",
            "--print",
            "--append-system-prompt",
            "be brief",
            "--",
            "--not an option",
        ]);
        parse(&args(&line))
    };
    assert_eq!(
        inspect(&["--dangerously-skip-permissions"]).unwrap_err(),
        "error: unknown option '--dangerously-skip-permissions'"
    );
    let inspect = inspect(&["--permission-mode", "bypassPermissions"]).unwrap();
    assert_eq!(
        inspect.permission_mode,
        Some(PermissionMode::BypassPermissions)
    );
    assert_eq!(inspect.prompt.as_deref(), Some("--not an option"));
    assert_eq!(
        inspect.session_id.as_deref(),
        Some("00000000-0000-4000-8000-000000000001")
    );
    assert_eq!(
        inspect.append_system_prompt.inline.as_deref(),
        Some("be brief")
    );
}

#[test]
fn system_prompt_and_directory_flags() {
    let invocation = parse(&args(&[
        "-p",
        "hi",
        "--system-prompt-file",
        "a.txt",
        "--append-system-prompt",
        "x",
        "--add-dir",
        "../lib",
        "/tmp/other",
        "--settings",
        "{}",
    ]))
    .unwrap();
    assert_eq!(invocation.system_prompt.file.as_deref(), Some("a.txt"));
    assert_eq!(invocation.system_prompt.inline, None);
    assert_eq!(invocation.append_system_prompt.inline.as_deref(), Some("x"));
    assert_eq!(invocation.add_dirs, ["../lib", "/tmp/other"]);
    assert_eq!(invocation.settings.as_deref(), Some("{}"));
}

/// `--session-id`（cli-args.md §7）：UUID 校验与续写时要 `--fork-session`，报错文字固定。
#[test]
fn session_id_is_checked() {
    let uuid = "00000000-0000-4000-8000-000000000001";
    assert_eq!(
        parse(&args(&["-p", "hi", "--session-id", "abc"])).unwrap_err(),
        "Error: Invalid session ID. Must be a valid UUID."
    );
    assert_eq!(
        parse(&args(&["-p", "hi", "--session-id", uuid, "-c"])).unwrap_err(),
        "Error: --session-id can only be used with --continue or --resume if --fork-session is also specified."
    );
    let forked = parse(&args(&[
        "-p",
        "hi",
        "--session-id",
        uuid,
        "-c",
        "--fork-session",
    ]))
    .unwrap();
    assert!(forked.fork_session);
    assert!(is_uuid("0A0B0C0D-0000-4000-8000-00000000000F"));
    assert!(!is_uuid("00000000-0000-4000-8000-00000000000"));
}

#[test]
fn continue_and_resume_pick_the_session() {
    let target = |values: &[&str]| parse(&args(values)).unwrap().target;
    assert_eq!(target(&["-p", "hi"]), SessionTarget::New);
    assert_eq!(target(&["-c", "-p", "hi"]), SessionTarget::Continue);
    assert_eq!(target(&["-p", "hi", "--continue"]), SessionTarget::Continue);
    assert_eq!(
        target(&["-r", "sess_1", "-p", "hi"]),
        SessionTarget::Resume(SessionId::new("sess_1"))
    );
    assert_eq!(
        target(&["--resume", "sess_1", "-p", "hi"]),
        SessionTarget::Resume(SessionId::new("sess_1"))
    );
    // `-r` 不带 ID 在 TUI 里打开选择器（见下一个测试），`-p` 里报错（固定的文字）。
    assert_eq!(
        parse(&args(&["-p", "hi", "-r"])).unwrap_err(),
        "Error: --resume requires a valid session ID or session title when used with --print. Usage: zcode -p --resume <session-id|title>"
    );
    assert!(parse(&args(&["-c", "-r", "sess_1", "-p", "hi"])).is_err());
}

#[test]
fn permission_and_turn_flags_parse() {
    let invocation = parse(&args(&[
        "-p",
        "hi",
        "--permission-mode",
        "dontAsk",
        "--allowedTools",
        "Read,Bash(git log:*) Write",
        "--disallowedTools",
        "Bash(rm:*)",
        "--max-turns",
        "3",
    ]))
    .unwrap();
    assert_eq!(invocation.permission_mode, Some(PermissionMode::DontAsk));
    assert_eq!(invocation.allowed, ["Read", "Bash(git log:*)", "Write"]);
    assert_eq!(invocation.disallowed, ["Bash(rm:*)"]);
    assert_eq!(invocation.max_turns, Some(3));
    for (name, mode) in [
        ("default", PermissionMode::Default),
        ("acceptEdits", PermissionMode::AcceptEdits),
        ("plan", PermissionMode::Plan),
        ("bypassPermissions", PermissionMode::BypassPermissions),
        ("auto", PermissionMode::Auto),
        // 本工程的名字（工作线 F63）。
        ("yolo", PermissionMode::BypassPermissions),
    ] {
        let parsed = parse(&args(&["--permission-mode", name, "-p", "x"])).unwrap();
        assert_eq!(parsed.permission_mode, Some(mode));
    }
    // 帮助里的 `manual` 与旧名 `default` 都是 default（cli-args.md §5.1）。
    let manual = parse(&args(&["--permission-mode", "manual", "-p", "x"])).unwrap();
    assert_eq!(manual.permission_mode, Some(PermissionMode::Default));
    // Node 的其余名字不认（cli-args.md §5.3：只加 `yolo`）。
    for node in ["build", "edit", "guarded"] {
        assert!(parse(&args(&["--permission-mode", node, "-p", "x"])).is_err());
    }
    assert!(parse(&args(&["--max-turns", "many", "-p", "x"])).is_err());
}

/// `--max-turns` 按 JS `Number()` 解析：`3abc` 不是数字；0 不限；小数向下取整；负数第一轮后就停。
#[test]
fn max_turns_follows_the_javascript_number_rules() {
    let turns = |value: &str| parse(&args(&["-p", "x", "--max-turns", value])).map(|i| i.max_turns);
    assert_eq!(turns("3"), Ok(Some(3)));
    assert_eq!(turns(" 4 "), Ok(Some(4)));
    assert_eq!(turns("2.5"), Ok(Some(2)));
    assert_eq!(turns("0x10"), Ok(Some(16)));
    assert_eq!(turns("1e1"), Ok(Some(10)));
    assert_eq!(turns("0"), Ok(None));
    assert_eq!(turns(""), Ok(None));
    assert_eq!(turns("-1"), Ok(Some(1)));
    assert_eq!(
        turns("3abc").unwrap_err(),
        "error: option '--max-turns <turns>' argument '3abc' is invalid. must be a number"
    );
    assert!(turns("inf").is_err());
    assert!(js_number("NaN").is_none());
    assert_eq!(js_number("-Infinity"), Some(f64::NEG_INFINITY));
}

#[test]
fn tool_lists_split_on_commas_and_spaces_outside_parentheses() {
    assert_eq!(
        split_tools("Bash(npm run test, lint),Read  Edit"),
        ["Bash(npm run test, lint)", "Read", "Edit"]
    );
    assert!(split_tools(" , ").is_empty());
}

#[test]
fn naming_glob_or_grep_opts_into_the_search_tools() {
    assert!(names_search_tools(&["Read".into(), "Glob".into()]));
    assert!(names_search_tools(&["Grep(*.rs)".into()]));
    assert!(!names_search_tools(&["Read".into(), "Bash(grep:*)".into()]));
}

/// `--resume` 不带 ID 时 TUI 打开会话选择器（`-p` 里要 ID）；`--fork-session` 只在续写时生效。
#[test]
fn resume_without_an_id_picks_and_fork_session_needs_a_target() {
    let picked = parse(&args(&["--resume"])).unwrap();
    assert!(picked.resume_pick);
    assert_eq!(picked.target, SessionTarget::New);
    let picked = parse(&args(&["--resume", "--fork-session"])).unwrap();
    assert!(picked.resume_pick && picked.fork_session);
    assert!(parse(&args(&["-p", "hi", "--resume"])).is_err());
    // 单独使用 `--fork-session` 不报错，忽略它、照常建新会话（session-lifecycle.md §1.1）。
    for alone in [&["--fork-session"][..], &["-p", "hi", "--fork-session"][..]] {
        let parsed = parse(&args(alone)).unwrap();
        assert!(!parsed.fork_session, "{alone:?}");
        assert_eq!(parsed.target, SessionTarget::New);
    }
    let forked = parse(&args(&["-p", "hi", "-c", "--fork-session"])).unwrap();
    assert!(forked.fork_session);
    assert_eq!(forked.target, SessionTarget::Continue);
}

#[test]
fn the_version_line_names_the_product_and_the_build() {
    assert_eq!(version_line("0.1.7"), "zcode 0.1.7 (rust-cli)");
}

/// 格式参数（headless.md §1，修复线 F39）：两种写法、布尔参数、choices 与组合的报错（文字固定）。
#[test]
fn format_flags_parse_and_check() {
    use rust_cli_headless::{InputFormat, OutputFormat};
    let format = |values: &[&str]| parse(&args(values)).unwrap().format;
    let streaming = format(&["--verbose", "--output-format", "stream-json", "-p", "hi"]);
    assert_eq!(streaming.output, OutputFormat::StreamJson);
    assert!(streaming.verbose && !streaming.partial);
    assert_eq!(
        format(&["--output-format=json", "-p", "hi"]).output,
        OutputFormat::Json
    );
    // stream-json 输入：`-p` 后面紧跟选项，没有 prompt；MCP 的 prewait 知道输入是 stream-json（headless.md §6.2）。
    let invocation = parse(&args(&[
        "-p",
        "--input-format",
        "stream-json",
        "--output-format=stream-json",
        "--verbose",
        "--include-partial-messages",
    ]))
    .unwrap();
    assert_eq!((invocation.mode, invocation.prompt), (Mode::Print, None));
    assert_eq!(invocation.format.input, InputFormat::StreamJson);
    assert!(invocation.format.partial && invocation.mcp.streaming_input);
    assert!(!parse(&args(&["-p", "hi"])).unwrap().mcp.streaming_input);
    // 值不在 choices 里、组合不合规：固定的报错文字。
    assert_eq!(
        parse(&args(&["-p", "hi", "--output-format", "xml"])).unwrap_err(),
        "error: option '--output-format <format>' argument 'xml' is invalid. Allowed choices are text, json, stream-json."
    );
    assert_eq!(
        parse(&args(&["-p", "hi", "--input-format", "yaml"])).unwrap_err(),
        "error: option '--input-format <format>' argument 'yaml' is invalid. Allowed choices are text, stream-json."
    );
    assert_eq!(
        parse(&args(&["-p", "--input-format", "stream-json"])).unwrap_err(),
        "Error: --input-format=stream-json requires output-format=stream-json."
    );
    assert_eq!(
        parse(&args(&[
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json"
        ]))
        .unwrap_err(),
        "Error: --input-format=stream-json requires --print."
    );
    assert_eq!(
        parse(&args(&["-p", "hi", "--include-partial-messages"])).unwrap_err(),
        "Error: --include-partial-messages requires --print and --output-format=stream-json."
    );
    assert_eq!(
        parse(&args(&["--output-format"])).unwrap_err(),
        "error: option '--output-format <format>' argument missing"
    );
}

/// `--effort`（cli-args.md §1.4）：解析时就定档位；不认识的值记一行警告、不报错，按缺省档位（后一个覆盖前一个）；
/// 缺值是 commander 的报错。Harbor 的写法 `--effort=high` 同样认。
#[test]
fn effort_is_a_level_or_a_warning() {
    let parsed = parse(&args(&["-p", "hi", "--effort", "high"])).unwrap();
    assert_eq!((parsed.effort, parsed.warnings.len()), (Some("high"), 0));
    let parsed = parse(&args(&["--effort=MED", "-p", "hi"])).unwrap();
    assert_eq!(parsed.effort, Some("medium"));
    let parsed = parse(&args(&["-p", "hi", "--effort", "max", "--effort", "Turbo"])).unwrap();
    assert_eq!(parsed.effort, None);
    assert_eq!(
        parsed.warnings,
        [
            "Warning: Unknown --effort value 'Turbo' \u{2014} ignoring it and using the default effort. Valid values: low, medium, high, xhigh, max."
        ]
    );
    assert_eq!(parsed.prompt.as_deref(), Some("hi"));
    assert_eq!(
        parse(&args(&["-p", "hi", "--effort"])).unwrap_err(),
        "error: option '--effort <level>' argument missing"
    );
}
