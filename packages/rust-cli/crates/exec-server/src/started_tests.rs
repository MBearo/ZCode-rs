//! 没被接走的流按客户端实例清掉；只有 `watch_command(op, 0)` 接走（工作线 F119）。

use futures_util::stream::{self, StreamExt};
use rust_cli_exec_wire::request::Request;

use super::Started;

fn watch(operation: &str, cursor: u64) -> Request {
    Request::WatchCommand {
        operation: operation.to_owned(),
        cursor,
    }
}

fn put(started: &Started, instance: &str, operation: &str) {
    started.table().insert(
        (instance.to_owned(), operation.to_owned()),
        stream::empty().boxed(),
    );
}

#[test]
fn only_the_first_watch_from_the_same_client_claims_the_stream() {
    let started = Started::default();
    put(&started, "a", "op1");
    assert!(started.claim("b", &watch("op1", 0)).is_none());
    assert!(started.claim("a", &watch("op1", 5)).is_none());
    assert!(started.claim("a", &watch("op1", 0)).is_some());
    assert!(started.claim("a", &watch("op1", 0)).is_none());
}

#[test]
fn a_forgotten_client_drops_its_unclaimed_streams() {
    let started = Started::default();
    put(&started, "a", "op1");
    put(&started, "b", "op2");
    started.forget("a");
    assert!(started.claim("a", &watch("op1", 0)).is_none());
    assert!(started.claim("b", &watch("op2", 0)).is_some());
}
