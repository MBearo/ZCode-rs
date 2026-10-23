//! `/mcp reconnect|enable|disable` 的前置检查与结果句（tui-manage.md §6.2）。
use super::*;

fn info(name: &str, state: McpServerState) -> McpServerInfo {
    McpServerInfo {
        name: name.to_owned(),
        state,
        reason: None,
        tools: 0,
    }
}

fn servers() -> Vec<McpServerInfo> {
    vec![
        info("docs", McpServerState::Connected),
        info("broken", McpServerState::Failed),
        info("off", McpServerState::Disabled),
    ]
}

fn say(args: &str, servers: &[McpServerInfo]) -> String {
    match plan(args, servers, Lang::En) {
        Plan::Say(text) => text,
        Plan::Run(run) => panic!("expected a message, got {run:?}"),
    }
}

fn run(args: &str) -> Run {
    match plan(args, &servers(), Lang::En) {
        Plan::Run(run) => run,
        Plan::Say(text) => panic!("expected a run, got {text}"),
    }
}

#[test]
fn checks_before_running_explain_the_problem() {
    assert_eq!(
        say("restart docs", &servers()),
        "\"restart\" isn't a recognized /mcp action. Try reconnect, enable, or disable."
    );
    assert_eq!(say("reconnect", &[]), "No MCP servers are configured.");
    assert_eq!(
        say("enable nope", &servers()),
        "There's no MCP server named \"nope\"."
    );
    assert_eq!(
        say("reconnect off", &servers()),
        "\"off\" is disabled. Run `/mcp enable off` to bring it back."
    );
    let pending = [info("docs", McpServerState::Pending)];
    assert!(say("reconnect docs", &pending).starts_with("\"docs\" is already reconnecting"));
    assert_eq!(
        say("enable docs", &servers()),
        "\"docs\" is already enabled."
    );
    assert_eq!(
        say("enable broken", &servers()),
        "\"broken\" is already enabled but not connected. Run `/mcp reconnect broken` to retry."
    );
    assert_eq!(
        say("disable off", &servers()),
        "\"off\" is already disabled."
    );
    let connected = [info("docs", McpServerState::Connected)];
    assert_eq!(
        say("reconnect all", &connected),
        "All enabled MCP servers are already connected or connecting."
    );
    let off = [info("off", McpServerState::Disabled)];
    assert_eq!(
        say("reconnect", &off),
        "1 MCP server(s) are disabled. Run `/mcp enable all` to bring them back."
    );
    assert_eq!(
        say("disable all", &off),
        "All MCP servers are already disabled."
    );
    // 要动的：reconnect all 只动失败的；enable all 只动停用的；disable all 动其余的。
    assert_eq!(run("reconnect").names, ["broken"]);
    assert_eq!(run("enable all").names, ["off"]);
    assert_eq!(run("disable all").names, ["docs", "broken"]);
    let one = run("disable docs");
    assert_eq!(one.one.as_deref(), Some("docs"));
    assert_eq!(one.persist(), Some(false));
    assert_eq!(run("reconnect broken").persist(), None);
}

#[test]
fn results_describe_what_changed() {
    let say = |args: &str, infos: &[McpServerInfo], failed: &[(String, String)]| {
        result(&run(args), infos, failed, Lang::En)
    };
    assert_eq!(
        say(
            "reconnect broken",
            &[info("broken", McpServerState::Connected)],
            &[]
        ),
        "Reconnected \"broken\"."
    );
    assert_eq!(
        say(
            "reconnect broken",
            &[info("broken", McpServerState::Failed)],
            &[]
        ),
        "Couldn't reconnect \"broken\" (not connected)."
    );
    assert_eq!(
        say("reconnect", &[info("broken", McpServerState::Failed)], &[]),
        "Reconnected 0 of 1 MCP server(s)."
    );
    assert_eq!(
        say(
            "disable docs",
            &[info("docs", McpServerState::Disabled)],
            &[]
        ),
        "Disabled \"docs\"."
    );
    assert_eq!(
        say(
            "disable docs",
            &[],
            &[("docs".to_owned(), "disk full".to_owned())]
        ),
        "Couldn't disable \"docs\" — disk full."
    );
    assert_eq!(
        say("enable off", &[info("off", McpServerState::Connected)], &[]),
        "Enabled \"off\"."
    );
    assert_eq!(
        say("enable off", &[info("off", McpServerState::Failed)], &[]),
        "Enabled \"off\", but it isn't connected yet."
    );
    assert_eq!(
        say(
            "disable all",
            &[info("docs", McpServerState::Disabled)],
            &[("broken".to_owned(), "x".to_owned())]
        ),
        "Disabled 1 MCP server(s) (1 couldn't be changed)."
    );
    assert_eq!(
        say("enable all", &[info("off", McpServerState::Failed)], &[]),
        "Enabled 1 MCP server(s) (1 enabled but not yet connected)."
    );
}
