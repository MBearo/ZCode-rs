// 复制自 rust/crates/kit/src/js/json_tests.rs（feat/rs 79b6691c55）。
use super::*;
use serde_json::json;

#[test]
fn numbers_follow_ecmascript() {
    let cases = [
        (1.0, "1"),
        (-0.0, "0"),
        (0.1, "0.1"),
        (1.5, "1.5"),
        (123_456_789.0, "123456789"),
        (1e21, "1e+21"),
        (1.5e21, "1.5e+21"),
        (1e20, "100000000000000000000"),
        (0.000001, "0.000001"),
        (1e-7, "1e-7"),
        (2.5e-8, "2.5e-8"),
        (-3.25, "-3.25"),
        (f64::NAN, "null"),
    ];
    for (value, expected) in cases {
        assert_eq!(js_double(value), expected, "{value}");
    }
    let parsed: Value = serde_json::from_str(r#"[1.0,2e2,-0.0,9007199254740993]"#).unwrap();
    assert_eq!(stringify(&parsed), "[1,200,0,9007199254740992]");
}

#[test]
fn objects_follow_js_property_order() {
    let parsed: Value =
        serde_json::from_str(r#"{"b":1,"10":2,"a":{"2":0,"x":1,"1":2},"01":3,"0":4}"#).unwrap();
    assert_eq!(
        stringify(&parsed),
        r#"{"0":4,"10":2,"b":1,"a":{"1":2,"2":0,"x":1},"01":3}"#
    );
    assert_eq!(
        stringify(&json!({"s":"a\"\\\n\u{1}\u{7f}/\u{2028}é"})),
        "{\"s\":\"a\\\"\\\\\\n\\u0001\u{7f}/\u{2028}é\"}"
    );
    assert_eq!(encode(&Value::Null), None);
    assert_eq!(encode(&json!({})).as_deref(), Some("{}"));
}

/// `utf16_len` 不生成字符串，结果必须与 `stringify` 后按 UTF-16 计长相等。
#[test]
fn utf16_len_matches_the_stringified_length() {
    let parsed: Value = serde_json::from_str(
        r#"{"b":1,"10":2,"a":{"2":0,"x":1.0,"1":-2.5e-8},"big":9007199254740993,"n":1e21,
        "neg":-9223372036854775808,"u":18446744073709551615,"z":-0.0,"t":true,"f":false,"0":null}"#,
    )
    .unwrap();
    let values = [
        parsed,
        json!("a\"\\\n\r\t\u{8}\u{c}\u{1}\u{1f}\u{7f}/\u{2028}é中文😀"),
        json!(["", [], {}, [null, [1, [2]]], {"k": {"k": "v"}}]),
        json!({"": "", "😀": "x", "\n": ["\u{0}"]}),
        Value::Null,
        json!(0),
        json!(-1),
        json!(0.1),
    ];
    for value in values {
        assert_eq!(
            utf16_len(&value),
            stringify(&value).encode_utf16().count(),
            "{value}"
        );
    }
    for text in ["", "plain", "\"\\", "\u{0}\u{1f} \u{7f}", "é中😀\u{10ffff}"] {
        assert_eq!(
            str_utf16_len(text),
            stringify(&json!(text)).encode_utf16().count(),
            "{text:?}"
        );
    }
}

/// 直接转义写出（修复线 F6）与 `serde_json` 的转义逐字节相同：全部 ASCII、多字节字符与长文本。
#[test]
fn strings_escape_exactly_like_serde_json() {
    let mut texts: Vec<String> = (0u8..=0x7f).map(|b| char::from(b).to_string()).collect();
    texts.push((0u8..=0x7f).map(char::from).collect());
    texts.extend(
        [
            "",
            "plain",
            "a\"b\\c\nd\re\tf\u{8}g\u{c}h",
            "é中文😀\u{2028}\u{2029}\u{10ffff}/",
            "\u{0}\u{1}\u{1e}\u{1f}\u{7f}\u{80}",
        ]
        .map(str::to_owned),
    );
    texts.push("x\n\"y\"\u{3}".repeat(1000));
    for text in texts {
        let mut out = String::from("prefix:");
        write_str(&mut out, &text);
        assert_eq!(
            out,
            format!("prefix:{}", serde_json::to_string(&text).unwrap()),
            "{text:?}"
        );
    }
}
