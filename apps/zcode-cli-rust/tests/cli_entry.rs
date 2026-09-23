use std::process::Command;

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_zcode-cli-rust"))
}

#[test]
fn tui_is_a_reserved_entry_that_fails_explicitly() {
    let output = binary().arg("tui").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    // stdout 只承载协议帧；占位入口不能向 stdout 写任何内容。
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("TUI"));
}

#[test]
fn app_server_still_requires_stdio_transport() {
    let output = binary().args(["app-server"]).output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}
