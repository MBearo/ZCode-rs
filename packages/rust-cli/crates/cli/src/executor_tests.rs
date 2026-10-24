use super::{ExecutorCommand, child_args, parse};

fn args(text: &[&str]) -> Vec<String> {
    text.iter().map(|arg| (*arg).to_owned()).collect()
}

fn ok(command: ExecutorCommand) -> Option<Result<ExecutorCommand, String>> {
    Some(Ok(command))
}

#[test]
fn only_the_executor_subcommand_is_taken() {
    assert_eq!(parse(&args(&["-p", "hi"])), None);
    assert_eq!(parse(&args(&[])), None);
}

/// remote-exec.md §19.3 的命令表。
#[test]
fn every_executor_command_parses() {
    assert_eq!(
        parse(&args(&["executor"])),
        ok(ExecutorCommand::Resident { daemon: false })
    );
    assert_eq!(
        parse(&args(&["executor", "--daemon"])),
        ok(ExecutorCommand::Resident { daemon: true })
    );
    assert_eq!(
        parse(&args(&["executor", "--stdio"])),
        ok(ExecutorCommand::Proxy)
    );
    assert_eq!(
        parse(&args(&["executor", "--stdio", "--standalone"])),
        ok(ExecutorCommand::Standalone {
            add_dirs: Vec::new(),
            find_and_grep: true,
        })
    );
    assert_eq!(
        parse(&args(&["executor", "status"])),
        ok(ExecutorCommand::Status { json: false })
    );
    assert_eq!(
        parse(&args(&["executor", "status", "--json"])),
        ok(ExecutorCommand::Status { json: true })
    );
    assert_eq!(
        parse(&args(&["executor", "stop"])),
        ok(ExecutorCommand::Stop { if_idle: false })
    );
    assert_eq!(
        parse(&args(&["executor", "stop", "--if-idle"])),
        ok(ExecutorCommand::Stop { if_idle: true })
    );
}

#[test]
fn the_loopback_arguments_round_trip() {
    let dirs = vec!["/a b".to_owned(), "/c".to_owned()];
    for find_and_grep in [true, false] {
        let child = child_args(&dirs, find_and_grep);
        // loopback 每个会话一个执行器（§18.6）：总是 `--standalone`，不经常驻进程。
        assert!(child.contains(&"--standalone".to_owned()));
        assert_eq!(
            parse(&child),
            ok(ExecutorCommand::Standalone {
                add_dirs: dirs.clone(),
                find_and_grep,
            })
        );
    }
}

#[test]
fn usage_errors_are_reported() {
    assert_eq!(
        parse(&args(&["executor", "--stdio", "--listen"])),
        Some(Err("error: unknown option '--listen'".to_owned()))
    );
    assert!(matches!(
        parse(&args(&["executor", "--stdio", "--standalone", "--add-dir"])),
        Some(Err(text)) if text.contains("argument missing")
    ));
    // 会话参数只能和 --standalone 一起用（常驻进程是几个会话共用的）。
    assert!(matches!(
        parse(&args(&["executor", "--stdio", "--add-dir", "/x"])),
        Some(Err(text)) if text.contains("--standalone")
    ));
    assert!(matches!(
        parse(&args(&["executor", "--no-find-and-grep"])),
        Some(Err(text)) if text.contains("--standalone")
    ));
    assert_eq!(
        parse(&args(&["executor", "status", "--all"])),
        Some(Err("error: unknown option '--all'".to_owned()))
    );
    assert_eq!(
        parse(&args(&["executor", "restart"])),
        Some(Err("error: unknown command 'restart'".to_owned()))
    );
}
