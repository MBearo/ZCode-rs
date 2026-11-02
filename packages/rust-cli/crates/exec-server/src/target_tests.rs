//! 分一次的去处与消息的种类一致：流的三种请求开流，别的不开（种类决定账本与补发，去处决定怎么执行）。

use std::sync::Arc;

use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::op::OpKind;
use rust_cli_exec_wire::request::Request;
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_kernel_api::mcp::NoMcpTransport;
use serde_json::json;

use super::{Mcp, McpCall, Port, Target};
use crate::test_support::TestMachine;

fn request(op: &str, args: serde_json::Value) -> Request {
    serde_json::from_value(json!({ "op": op, "args": args })).unwrap()
}

fn machine() -> MachineEnv {
    Arc::new(TestMachine::default()).machine(Arc::new(NoMcpTransport))
}

#[tokio::test]
async fn streams_are_exactly_the_stream_kinds() {
    let workspace = json!({ "environment": "local", "root": "/w" });
    let requests = [
        request(
            "commands.watch_command",
            json!({ "operation": "op1", "cursor": 0 }),
        ),
        request(
            "shell.shell_notices",
            json!({ "session": "s1", "after": 0 }),
        ),
        request("mcp.events", json!({ "connection": "c1" })),
        request("info.facts", json!({ "workspace": workspace })),
        request("commands.cancel", json!({ "operation": "op1" })),
        request("mcp.send", json!({ "connection": "c1", "message": {} })),
        request("mcp.close", json!({ "connection": "c1" })),
        request("env.update", json!({ "mode": "merge", "vars": {} })),
    ];
    let machine = machine();
    for request in requests {
        let op = request.op();
        let target = Target::of(request, &machine, &Body::empty());
        assert_eq!(target.is_stream(), op.kind() == OpKind::Stream, "{op:?}");
    }
}

#[tokio::test]
async fn each_class_gets_its_own_arguments() {
    let machine = machine();
    let watch = request(
        "commands.watch_command",
        json!({ "operation": "op1", "cursor": 3 }),
    );
    assert!(matches!(
        Target::of(watch, &machine, &Body::empty()),
        Target::Port(Port::Watch { operation, cursor: 3 }) if operation == "op1"
    ));
    let close = request("mcp.close", json!({ "connection": "c1" }));
    assert!(matches!(
        Target::of(close, &machine, &Body::empty()),
        Target::Mcp(Mcp::Call(McpCall::Close { connection })) if connection == "c1"
    ));
    let env = request(
        "env.update",
        json!({ "mode": "merge", "vars": {}, "injected": ["SSH_AUTH_SOCK"] }),
    );
    assert!(matches!(
        Target::of(env, &machine, &Body::empty()),
        Target::Env(push) if push.injected == ["SSH_AUTH_SOCK"]
    ));
}
