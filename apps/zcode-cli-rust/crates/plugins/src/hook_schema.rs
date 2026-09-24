//! Node `HookMatcherConfigSchema` (zod, non-strict: unknown keys are
//! dropped). Issues use zod 4's `path: message` wording for the common cases.
use serde_json::{Map, Value};

fn kind(value: Option<&Value>) -> &'static str {
    match value {
        None => "undefined",
        Some(Value::Null) => "null",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
        Some(Value::String(_)) => "string",
        Some(Value::Number(_)) => "number",
        Some(Value::Bool(_)) => "boolean",
    }
}

struct Issues(Vec<String>);

impl Issues {
    fn push(&mut self, path: &str, message: String) {
        self.0.push(format!("{path}: {message}"));
    }
    fn expected(&mut self, path: &str, expected: &str, value: Option<&Value>) {
        self.push(
            path,
            format!(
                "Invalid input: expected {expected}, received {}",
                kind(value)
            ),
        );
    }
}

fn string(object: &Map<String, Value>, key: &str, path: &str, min: bool, issues: &mut Issues) {
    match object.get(key) {
        None if !min => {}
        Some(Value::String(s)) if !min || !s.is_empty() => {}
        Some(Value::String(_)) => issues.push(
            &format!("{path}.{key}"),
            "Too small: expected string to have >=1 characters".into(),
        ),
        other => issues.expected(&format!("{path}.{key}"), "string", other),
    }
}

fn boolean(object: &Map<String, Value>, key: &str, path: &str, issues: &mut Issues) {
    if let Some(value) = object.get(key).filter(|v| !v.is_boolean()) {
        issues.expected(&format!("{path}.{key}"), "boolean", Some(value));
    }
}

fn positive(object: &Map<String, Value>, key: &str, path: &str, int: bool, issues: &mut Issues) {
    let Some(value) = object.get(key) else {
        return;
    };
    let path = format!("{path}.{key}");
    let Some(number) = value.as_f64() else {
        return issues.expected(&path, "number", Some(value));
    };
    if int && number.fract() != 0.0 {
        issues.expected(&path, "int", Some(value));
    } else if number <= 0.0 {
        issues.push(&path, "Too small: expected number to be >0".into());
    }
}

const PROCESS_KEYS: [&str; 6] = [
    "type",
    "command",
    "enabled",
    "args",
    "timeoutMs",
    "statusMessage",
];
const COMMAND_KEYS: [&str; 8] = [
    "type",
    "command",
    "enabled",
    "async",
    "shell",
    "timeout",
    "timeoutMs",
    "statusMessage",
];

fn hook(value: &Value, path: &str, issues: &mut Issues) -> Option<Value> {
    let Value::Object(object) = value else {
        issues.expected(path, "object", Some(value));
        return None;
    };
    let before = issues.0.len();
    let keys: &[&str] = match object.get("type").and_then(Value::as_str) {
        Some("process") => {
            string(object, "command", path, true, issues);
            boolean(object, "enabled", path, issues);
            match object.get("args") {
                None => {}
                Some(Value::Array(args)) => {
                    for (i, arg) in args.iter().enumerate() {
                        if !arg.is_string() {
                            issues.expected(&format!("{path}.args.{i}"), "string", Some(arg));
                        }
                    }
                }
                other => issues.expected(&format!("{path}.args"), "array", other),
            }
            positive(object, "timeoutMs", path, true, issues);
            string(object, "statusMessage", path, false, issues);
            &PROCESS_KEYS
        }
        Some("command") => {
            string(object, "command", path, true, issues);
            boolean(object, "enabled", path, issues);
            boolean(object, "async", path, issues);
            match object.get("shell") {
                None | Some(Value::Bool(true)) => {}
                Some(Value::String(s)) if !s.is_empty() => {}
                Some(_) => issues.push(&format!("{path}.shell"), "Invalid input".into()),
            }
            positive(object, "timeout", path, false, issues);
            positive(object, "timeoutMs", path, true, issues);
            string(object, "statusMessage", path, false, issues);
            &COMMAND_KEYS
        }
        _ => {
            issues.push(&format!("{path}.type"), "Invalid input".into());
            return None;
        }
    };
    (issues.0.len() == before).then(|| {
        Value::Object(
            keys.iter()
                .filter_map(|k| object.get(*k).map(|v| ((*k).to_owned(), v.clone())))
                .collect(),
        )
    })
}

/// The validated matcher (`{matcher?, hooks}`) or the joined issues.
pub fn matcher(value: &Value) -> Result<Value, String> {
    let mut issues = Issues(vec![]);
    let Value::Object(object) = value else {
        issues.expected("", "object", Some(value));
        return Err(issues.0.join("; "));
    };
    let mut result = Map::new();
    match object.get("matcher") {
        None => {}
        Some(Value::String(m)) => {
            result.insert("matcher".into(), m.clone().into());
        }
        other => issues.expected("matcher", "string", other),
    }
    let mut hooks = vec![];
    match object.get("hooks") {
        Some(Value::Array(items)) => {
            if items.is_empty() {
                issues.push(
                    "hooks",
                    "Too small: expected array to have >=1 items".into(),
                );
            }
            for (i, item) in items.iter().enumerate() {
                if let Some(valid) = hook(item, &format!("hooks.{i}"), &mut issues) {
                    hooks.push(valid);
                }
            }
        }
        other => issues.expected("hooks", "array", other),
    }
    if !issues.0.is_empty() {
        return Err(issues.0.join("; "));
    }
    result.insert("hooks".into(), Value::Array(hooks));
    Ok(Value::Object(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn matchers_are_validated_and_stripped_like_zod() {
        let valid = matcher(&json!({"matcher":"Bash","extra":1,"hooks":[
            {"type":"command","command":"echo","shell":true,"x":1},
            {"type":"process","command":"node","args":["a"],"timeoutMs":5}]}))
        .unwrap();
        assert_eq!(
            valid,
            json!({"matcher":"Bash","hooks":[{"type":"command","command":"echo","shell":true},
                {"type":"process","command":"node","args":["a"],"timeoutMs":5}]})
        );
        assert_eq!(
            matcher(&json!({"hooks":[]})).unwrap_err(),
            "hooks: Too small: expected array to have >=1 items"
        );
        assert_eq!(
            matcher(&json!({"hooks":[{"type":"x"},{"type":"command","command":"","timeout":0}]}))
                .unwrap_err(),
            "hooks.0.type: Invalid input; hooks.1.command: Too small: expected string to have >=1 characters; hooks.1.timeout: Too small: expected number to be >0"
        );
        assert_eq!(
            matcher(&json!({"matcher":1})).unwrap_err(),
            "matcher: Invalid input: expected string, received number; hooks: Invalid input: expected array, received undefined"
        );
    }
}
