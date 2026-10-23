use super::*;

#[test]
fn a_user_message_with_string_or_text_blocks_is_a_prompt() {
    assert_eq!(
        parse(r#"{"type":"user","message":{"role":"user","content":"Say hello."}}"#),
        Ok(Line::Prompt("Say hello.".to_owned()))
    );
    assert_eq!(
        parse(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"a"},{"type":"image"},{"type":"text","text":"b"}]},"session_id":"x","parent_tool_use_id":null}"#
        ),
        Ok(Line::Prompt("a\n\nb".to_owned()))
    );
}

/// 空行、不是 user 的消息（`assistant`、自定义的 `foo`、`control_request`）都忽略。
#[test]
fn other_lines_are_skipped() {
    for line in [
        "",
        "   ",
        r#"{"type":"assistant","message":{"role":"assistant","content":"x"}}"#,
        r#"{"type":"foo"}"#,
        r#"{"type":"control_request","request_id":"r1","request":{"subtype":"interrupt"}}"#,
        "123",
    ] {
        assert_eq!(parse(line), Ok(Line::Skip), "{line}");
    }
}

/// 报错的原文：不是 JSON 时报长度（JS 字符串长度）；role 不是 user 时报出 role，没有时是 `undefined`。
#[test]
fn malformed_lines_report_the_fixed_errors() {
    assert_eq!(
        parse("not json"),
        Err("Error parsing streaming input line (type=unknown, 8 chars): SyntaxError".to_owned())
    );
    assert_eq!(
        parse("é{"),
        Err("Error parsing streaming input line (type=unknown, 2 chars): SyntaxError".to_owned())
    );
    assert_eq!(
        parse(r#"{"type":"user","message":{"role":"assistant","content":"x"}}"#),
        Err("Error: Expected message role 'user', got 'assistant'".to_owned())
    );
    assert_eq!(
        parse(r#"{"type":"user","message":{"content":"x"}}"#),
        Err("Error: Expected message role 'user', got 'undefined'".to_owned())
    );
    assert_eq!(
        parse(r#"{"type":"user"}"#),
        Err("Error: Expected message role 'user', got 'undefined'".to_owned())
    );
}
