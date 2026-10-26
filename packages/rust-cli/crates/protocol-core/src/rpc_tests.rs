use serde_json::json;

use super::*;

#[test]
fn requests_notifications_and_responses_are_told_apart() {
    assert_eq!(
        parse(r#"{"id":1,"method":"a/b","params":{"x":1},"trace":{}}"#),
        Ok(Incoming::Request {
            id: json!(1),
            method: "a/b".to_owned(),
            params: json!({"x": 1}),
        })
    );
    assert_eq!(
        parse(r#"{"method":"a/n"}"#),
        Ok(Incoming::Notification {
            method: "a/n".to_owned(),
            params: Value::Null,
        })
    );
    assert_eq!(
        parse(r#"{"id":"rs-1","result":{"ok":true}}"#),
        Ok(Incoming::Response {
            id: json!("rs-1"),
            result: Ok(json!({"ok": true})),
        })
    );
    let Ok(Incoming::Response { result, .. }) =
        parse(r#"{"id":"rs-2","error":{"code":-32601,"message":"Method not found: x"}}"#)
    else {
        panic!("an error response");
    };
    assert_eq!(result.unwrap_err().code, codes::METHOD_NOT_FOUND);
}

#[test]
fn malformed_lines_carry_the_id_only_when_one_is_known() {
    let error = parse("not json").unwrap_err();
    assert_eq!((error.id, error.error.code), (None, codes::PARSE_ERROR));
    let error = parse(r#"{"id":3}"#).unwrap_err();
    assert_eq!(
        (error.id, error.error.code),
        (Some(json!(3)), codes::INVALID_REQUEST)
    );
    let error = parse(r#"{"id":{"x":1},"method":"m"}"#).unwrap_err();
    assert_eq!((error.id, error.error.code), (None, codes::INVALID_REQUEST));
    assert_eq!(parse("[1]").unwrap_err().error.code, codes::INVALID_REQUEST);
}

#[test]
fn lines_have_no_protocol_marker_and_echo_the_id() {
    assert_eq!(
        response(&json!("7"), &Ok(json!({}))),
        r#"{"id":"7","result":{}}"#
    );
    assert_eq!(
        response(&json!(7), &Err(RpcError::method_not_found("x/y"))),
        r#"{"id":7,"error":{"code":-32601,"message":"Method not found: x/y"}}"#
    );
    assert_eq!(
        response(
            &json!(8),
            &Err(RpcError::invalid_params("bad").with_data(json!({"path": "/a"})))
        ),
        r#"{"id":8,"error":{"code":-32602,"message":"bad","data":{"path":"/a"}}}"#
    );
    assert_eq!(
        request(&json!("rs-1"), "m", &json!({})),
        r#"{"id":"rs-1","method":"m","params":{}}"#
    );
    assert_eq!(
        notification("n", &json!([])),
        r#"{"method":"n","params":[]}"#
    );
}
