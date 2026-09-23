//! Tests for the zod-subset JSON Schema validator.
use super::*;
use serde_json::json;

#[test]
fn strip_matches_zod_output_modes() {
    let node = Node::compile(&json!({
        "type":"object",
        "properties":{
            "strict":{"type":"object","properties":{"a":{"type":"number"}}},
            "loose":{"type":"object","properties":{"a":{"type":"number"}},"additionalProperties":{}},
            "any":{}
        },
        "additionalProperties":{}
    }))
    .unwrap();
    let value = json!({"strict":{"a":1,"x":2},"loose":{"a":1,"x":2},"any":{"x":2},"top":true});
    assert_eq!(
        node.strip(&value),
        json!({"strict":{"a":1},"loose":{"a":1,"x":2},"any":{"x":2},"top":true})
    );
}

#[test]
fn rejects_unsupported_keywords_at_compile_time() {
    assert!(Node::compile(&json!({"type":"string","allOf":[]})).is_err());
    assert!(Node::compile(&json!({"type":"string","format":"email"})).is_err());
}

#[test]
fn validates_zod_subset_semantics() {
    let node = Node::compile(&json!({
        "type":"object",
        "properties":{
            "name":{"type":"string","minLength":1,"maxLength":2},
            "count":{"type":"integer","exclusiveMinimum":0,"maximum":100},
            "mode":{"type":"string","enum":["a","b"]},
            "tags":{"type":"array","items":{"type":"string"},"maxItems":1},
            "either":{"anyOf":[{"type":"string"},{"type":"null"}]},
            "one":{"oneOf":[{"type":"object","properties":{"k":{"const":"a"}},"required":["k"]},{"type":"object","properties":{"k":{"const":"b"}},"required":["k"]}]},
            "map":{"type":"object","propertyNames":{"type":"string","minLength":2},"additionalProperties":{"type":"number"}}
        },
        "required":["name"],
        "additionalProperties":false
    }))
    .unwrap();
    assert!(
        node.validate(
            &json!({"name":"ok","count":3,"mode":"a","tags":["x"],"either":null,"map":{"kk":1}}),
            ""
        )
        .is_ok()
    );
    for bad in [
        json!({}),
        json!({"name":""}),
        json!({"name":"😀😀"}),
        json!({"name":"a","count":0}),
        json!({"name":"a","count":1.5}),
        json!({"name":"a","mode":"c"}),
        json!({"name":"a","tags":["x","y"]}),
        json!({"name":"a","either":1}),
        json!({"name":"a","map":{"k":1}}),
        json!({"name":"a","map":{"kk":"x"}}),
        json!({"name":"a","extra":true}),
        json!({"name":"a","one":{"k":"c"}}),
    ] {
        assert!(node.validate(&bad, "").is_err(), "{bad}");
    }
}
