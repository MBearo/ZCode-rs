//! Parity with the shared zod schemas and Node `parseParams`. Fixtures come from
//! `scripts/zcode-cli-rust-legacy-params-fixtures.mjs`.
use super::*;
use serde_json::Value;

fn fixtures() -> Value {
    serde_json::from_str(include_str!("../fixtures/legacy-params.json")).unwrap()
}

/// serde_json 按字母序保存对象键，多个未知键的顺序可能与 JS 插入顺序不同；
/// 比对时把 `keys` 与对应文本归一，其余字段逐字比较。
fn normalize(value: &mut Value) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(normalize),
        Value::Object(map) => {
            if map.get("code") == Some(&Value::from("unrecognized_keys"))
                && let Some(Value::Array(keys)) = map.get_mut("keys")
            {
                keys.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
                let quoted: Vec<String> = keys.iter().map(|k| format!("{k}")).collect();
                let noun = if keys.len() == 1 { "key" } else { "keys" };
                map["message"] = format!("Unrecognized {noun}: {}", quoted.join(", ")).into();
            }
            map.values_mut().for_each(normalize);
        }
        _ => {}
    }
}

fn issues(data: &Value) -> Value {
    let mut parsed: Value = serde_json::from_str(data["message"].as_str().unwrap()).unwrap();
    normalize(&mut parsed);
    parsed
}

fn check(parse: fn(&Value) -> Result<Value, ParamsError>, cases: &Value) {
    for case in cases.as_array().unwrap() {
        // transport 无法区分缺省与显式 null，根 null 一律视为缺省（已知差异）。
        if case["input"].is_null() && case["missing"] == false {
            continue;
        }
        match (parse(&case["input"]), case.get("ok")) {
            (Ok(parsed), Some(ok)) => assert_eq!(&parsed, ok, "{}", case["input"]),
            (Err(error), None) => {
                let expected = &case["error"];
                assert_eq!(error.data["name"], expected["data"]["name"]);
                assert_eq!(
                    issues(&error.data),
                    issues(&expected["data"]),
                    "{}",
                    case["input"]
                );
                let plain = !expected["data"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("Unrecognized keys");
                if plain {
                    assert_eq!(error.message, expected["message"], "{}", case["input"]);
                    assert_eq!(error.data, expected["data"], "{}", case["input"]);
                }
            }
            (actual, _) => panic!("{} => {actual:?}", case["input"]),
        }
    }
}

#[test]
fn create_params_match_zod() {
    check(create, &fixtures()["create"]);
}

#[test]
fn resume_params_match_zod() {
    check(resume, &fixtures()["resume"]);
}
