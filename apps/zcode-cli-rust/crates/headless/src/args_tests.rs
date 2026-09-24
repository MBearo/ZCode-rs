use super::*;

fn args(line: &[&str]) -> Vec<String> {
    line.iter().map(|s| (*s).to_owned()).collect()
}

fn prompt(line: &[&str]) -> PromptArgs {
    match parse(&args(line)) {
        Ok(Parsed::Prompt(p)) => *p,
        other => panic!("{line:?} => {other:?}"),
    }
}

fn error(line: &[&str]) -> ArgError {
    parse(&args(line)).expect_err("should fail")
}

#[test]
fn prompt_defaults_and_overrides() {
    let p = prompt(&["-p", "hi", "extra", "words"]);
    assert_eq!(p.prompt, "hi");
    assert_eq!(p.format, Format::Text);
    assert_eq!(p.mode, "yolo");
    assert_eq!(p.resume, Resume::New);
    assert_eq!(p.surface, "terminal");
    let p = prompt(&[
        "--prompt=-dash",
        "--json",
        "--mode",
        "PLAN",
        "-c",
        "--verbose",
    ]);
    assert_eq!(p.prompt, "-dash");
    assert_eq!((p.format, p.mode.as_str()), (Format::Json, "plan"));
    assert_eq!(p.resume, Resume::Latest);
    assert!(p.verbose);
    let p = prompt(&[
        "-p-x",
        "--json",
        "--output-format",
        "text",
        "--surface",
        "Desktop",
    ]);
    assert_eq!((p.prompt.as_str(), p.format), ("-x", Format::Text));
    assert_eq!(p.surface, "desktop");
    let p = prompt(&[
        "-p",
        "hi",
        "--attach",
        "a.png",
        "--attach=b.txt",
        "--resume",
        "s1",
    ]);
    assert_eq!(p.attach, ["a.png", "b.txt"]);
    assert_eq!(p.resume, Resume::Id("s1".into()));
    assert_eq!(
        prompt(&["-p", "x", "--output-format", "stream-json"]).format,
        Format::StreamJson
    );
}

#[test]
fn disallowed_tools_are_greedy_and_normalized() {
    let p = prompt(&[
        "--disallowed-tools",
        "Bash,Edit",
        "Write Bash(git *, x)",
        "web_search",
        "-p",
        "hi",
        "--disallowedTools=web_search(a),Edit",
    ]);
    assert_eq!(
        p.disallowed_tools,
        [
            "Bash",
            "Edit",
            "Write",
            "Bash(git *, x)",
            "WebSearch",
            "WebSearch(a)"
        ]
    );
    assert_eq!(
        error(&["-p", "hi", "--disallowed-tools", "--json"]),
        ArgError {
            message: "--disallowed-tools requires at least one tool.".into(),
            help: true
        }
    );
}

#[test]
fn parse_errors_match_node() {
    let cases: [(&[&str], &str); 5] = [
        (
            &["-p", "hi", "--bogus=1"],
            "Unknown option '--bogus'. To specify a positional argument starting with a '-', place it at the end of the command after '--', as in '-- \"--bogus\"",
        ),
        (&["-p"], "Option '-p, --prompt <value>' argument missing"),
        (
            &["-p", "hi", "--cwd"],
            "Option '--cwd <value>' argument missing",
        ),
        (
            &["-p", "-dash"],
            "Option '-p' argument is ambiguous.\nDid you forget to specify the option argument for '-p'?\nTo specify an option argument starting with a dash use '--prompt=-XYZ' or '-p-XYZ'.",
        ),
        (
            &["-p", "hi", "--json=1"],
            "Option '--json' does not take an argument",
        ),
    ];
    for (line, message) in cases {
        assert_eq!(
            error(line),
            ArgError {
                message: message.into(),
                help: true
            },
            "{line:?}"
        );
    }
    assert_eq!(
        error(&["-xp", "hi"]).message.lines().next().unwrap(),
        "Unknown option '-x'. To specify a positional argument starting with a '-', place it at the end of the command after '--', as in '-- \"-x\""
    );
}

#[test]
fn validation_order_and_texts_match_node() {
    let cases: [(&[&str], &str); 10] = [
        (
            &["-p", "x", "--locale", "xx", "--mode", "foo"],
            "Unsupported --locale value: xx. Supported locales: en-US, zh-CN, auto.",
        ),
        (
            &["-p", "x", "--mode", "auto"],
            "Unsupported --mode value: auto. Supported modes: build, edit, plan, yolo.",
        ),
        (
            &["-p", "x", "--browser-use", "x"],
            "Unsupported --browser-use value: x. Supported value: headless.",
        ),
        (
            &["-p", "x", "--surface", "web"],
            "Unsupported --surface value: web. Supported surfaces: terminal, desktop.",
        ),
        (
            &["-p", "x", "--resume", "s", "-c"],
            "--resume and --continue cannot be used together.",
        ),
        (
            &["-p", "x", "--output-format", "xml"],
            "--output-format must be one of text, json, stream-json (received: xml).",
        ),
        (
            &["-p", "x", "--target", "goal"],
            "--target cannot be used with --prompt. Use either --target <objective> or --prompt \"/goal <objective>\".",
        ),
        (
            &["--surface", "desktop"],
            "--surface can only be used with --prompt, --target, app-server, or agent-server.",
        ),
        (
            &["--memory-bench"],
            "--memory-bench can only be used with -p/--prompt.",
        ),
        (
            &["-p", "x", "--force-mcs"],
            "--force-mcs is not supported by the Rust runtime yet.",
        ),
    ];
    for (line, message) in cases {
        assert_eq!(
            error(line),
            ArgError {
                message: message.into(),
                help: false
            },
            "{line:?}"
        );
    }
    assert_eq!(
        check_prompt("  ").unwrap_err().message,
        "--prompt requires non-empty text."
    );
    assert!(check_prompt(" x ").is_ok());
}

#[test]
fn help_and_version_come_before_the_prompt() {
    assert_eq!(parse(&args(&["-p", "hi", "--help"])), Ok(Parsed::Help));
    assert_eq!(parse(&args(&["-v"])), Ok(Parsed::Version));
    assert_eq!(parse(&args(&["-hv"])), Ok(Parsed::Help));
    assert_eq!(parse(&args(&[])), Ok(Parsed::Tui));
    // 帮助之前的校验仍然先报错（Node run.ts 的顺序）。
    assert!(parse(&args(&["--mode", "x", "--help"])).is_err());
}
