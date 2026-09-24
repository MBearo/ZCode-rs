//! zod 4 run semantics for [`Schema`](super::Schema).
use super::{
    Format, Issue, J, Parsed, Schema, aborted, invalid_type, js_trim, too_big, too_small,
};
use serde_json::{Map, Value};

const MAX_SAFE: i64 = 9_007_199_254_740_991;

fn prefix(issues: Vec<Issue>, segment: J) -> Vec<Issue> {
    issues
        .into_iter()
        .map(|i| i.prefixed(segment.clone()))
        .collect()
}

fn fail(issue: Issue) -> Parsed {
    (Value::Null, vec![issue])
}

pub(super) fn run(schema: &Schema, value: Option<&Value>) -> Parsed {
    match schema {
        Schema::String {
            trim,
            min,
            max,
            format,
        } => string(value, *trim, (*min, *max), format.as_ref()),
        Schema::Bool => match value {
            Some(Value::Bool(b)) => (Value::Bool(*b), vec![]),
            _ => fail(invalid_type("boolean", value)),
        },
        Schema::Int(minimum, maximum) => int(value, *minimum, *maximum),
        Schema::Enum(values) => match value {
            Some(Value::String(s)) if values.contains(&s.as_str()) => {
                (Value::String(s.clone()), vec![])
            }
            _ => {
                let list = values
                    .iter()
                    .map(|v| format!("\"{v}\""))
                    .collect::<Vec<_>>();
                fail(Issue::new(
                    vec![
                        ("code", J::S("invalid_value".into())),
                        (
                            "values",
                            J::A(values.iter().map(|v| J::S((*v).into())).collect()),
                        ),
                    ],
                    format!("Invalid option: expected one of {}", list.join("|")),
                    true,
                ))
            }
        },
        Schema::Literal(expected) => match value {
            Some(v) if v == expected => (v.clone(), vec![]),
            _ => fail(Issue::new(
                vec![
                    ("code", J::S("invalid_value".into())),
                    ("values", J::A(vec![J::V(expected.clone())])),
                ],
                format!("Invalid input: expected {expected}"),
                true,
            )),
        },
        Schema::Array(item, min) => array(value, item, *min),
        Schema::Object(fields) => object(value, fields),
        Schema::Union(options) => union(value, options),
        Schema::Discriminated(key, options) => discriminated(value, key, options),
        Schema::Record(key, item) => record(value, key, item.as_deref()),
        Schema::Refined(inner, check) => {
            let (parsed, mut issues) = run(inner, value);
            if !aborted(&issues) {
                issues.extend(check(&parsed));
            }
            (parsed, issues)
        }
    }
}

fn string(
    value: Option<&Value>,
    trim: bool,
    (min, max): (Option<usize>, Option<usize>),
    format: Option<&Format>,
) -> Parsed {
    let Some(Value::String(raw)) = value else {
        return fail(invalid_type("string", value));
    };
    let text = if trim { js_trim(raw) } else { raw.as_str() };
    let mut issues = vec![];
    if let Some(min) = min
        && text.encode_utf16().count() < min
    {
        issues.push(too_small(
            "string",
            min as i64,
            true,
            format!("Too small: expected string to have >={min} characters"),
        ));
    }
    if let Some(max) = max
        && text.encode_utf16().count() > max
    {
        issues.push(too_big(
            "string",
            max as i64,
            format!("Too big: expected string to have <={max} characters"),
        ));
    }
    match format {
        Some(Format::Hex64)
            if !(text.len() == 64
                && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))) =>
        {
            let pattern = "/^[0-9a-f]{64}$/u";
            issues.push(Issue::new(
                vec![
                    ("origin", J::S("string".into())),
                    ("code", J::S("invalid_format".into())),
                    ("format", J::S("regex".into())),
                    ("pattern", J::S(pattern.into())),
                ],
                format!("Invalid string: must match pattern {pattern}"),
                false,
            ));
        }
        Some(Format::Url) if url::Url::parse(text).is_err() => issues.push(Issue::new(
            vec![
                ("code", J::S("invalid_format".into())),
                ("format", J::S("url".into())),
            ],
            "Invalid URL".into(),
            false,
        )),
        _ => {}
    }
    (Value::String(text.to_owned()), issues)
}

/// `z.number().int()` plus an optional `nonnegative` / `positive` bound and `max`.
fn int(value: Option<&Value>, minimum: Option<(i64, bool)>, maximum: Option<i64>) -> Parsed {
    let Some(Value::Number(number)) = value else {
        return fail(invalid_type("number", value));
    };
    let n = number.as_f64().unwrap_or(f64::NAN);
    if n.fract() != 0.0 || !n.is_finite() {
        return fail(Issue::new(
            vec![
                ("expected", J::S("int".into())),
                ("format", J::S("safeint".into())),
                ("code", J::S("invalid_type".into())),
            ],
            "Invalid input: expected int, received number".into(),
            true,
        ));
    }
    let mut issues = vec![];
    let note = J::S("Integers must be within the safe integer range.".into());
    if n > MAX_SAFE as f64 {
        issues.push(Issue::new(
            vec![
                ("code", J::S("too_big".into())),
                ("maximum", J::N(MAX_SAFE)),
                ("note", note.clone()),
                ("origin", J::S("int".into())),
                ("inclusive", J::V(Value::Bool(true))),
            ],
            format!("Too big: expected int to be <={MAX_SAFE}"),
            false,
        ));
    } else if n < -(MAX_SAFE as f64) {
        issues.push(Issue::new(
            vec![
                ("code", J::S("too_small".into())),
                ("minimum", J::N(-MAX_SAFE)),
                ("note", note),
                ("origin", J::S("int".into())),
                ("inclusive", J::V(Value::Bool(true))),
            ],
            format!("Too small: expected int to be >={}", -MAX_SAFE),
            false,
        ));
    }
    if let Some((min, inclusive)) = minimum
        && (if inclusive {
            n < min as f64
        } else {
            n <= min as f64
        })
    {
        let op = if inclusive { ">=" } else { ">" };
        issues.push(too_small(
            "number",
            min,
            inclusive,
            format!("Too small: expected number to be {op}{min}"),
        ));
    }
    if let Some(max) = maximum.filter(|max| n > *max as f64) {
        issues.push(too_big(
            "number",
            max,
            format!("Too big: expected number to be <={max}"),
        ));
    }
    (Value::Number(number.clone()), issues)
}

fn array(value: Option<&Value>, item: &Schema, min: Option<usize>) -> Parsed {
    let Some(Value::Array(items)) = value else {
        return fail(invalid_type("array", value));
    };
    let mut out = vec![];
    let mut issues = vec![];
    for (index, element) in items.iter().enumerate() {
        let (parsed, found) = run(item, Some(element));
        out.push(parsed);
        issues.extend(prefix(found, J::N(index as i64)));
    }
    if let Some(min) = min
        && items.len() < min
    {
        issues.push(too_small(
            "array",
            min as i64,
            true,
            format!("Too small: expected array to have >={min} items"),
        ));
    }
    (Value::Array(out), issues)
}

fn object(value: Option<&Value>, fields: &[(&'static str, Schema, bool)]) -> Parsed {
    let Some(Value::Object(input)) = value else {
        return fail(invalid_type("object", value));
    };
    let mut out = Map::new();
    let mut issues = vec![];
    for (key, schema, optional) in fields {
        let field = input.get(*key);
        if field.is_none() && *optional {
            continue;
        }
        let (parsed, found) = run(schema, field);
        if field.is_some() {
            out.insert((*key).into(), parsed);
        }
        issues.extend(prefix(found, J::S((*key).into())));
    }
    // serde_json 的对象按键排序，多个未知键的顺序可能与 JS 插入顺序不同（已知差异）。
    let unknown: Vec<&String> = input
        .keys()
        .filter(|k| !fields.iter().any(|(f, _, _)| f == k))
        .collect();
    if !unknown.is_empty() {
        let quoted = unknown
            .iter()
            .map(|k| format!("\"{k}\""))
            .collect::<Vec<_>>();
        let noun = if unknown.len() == 1 { "key" } else { "keys" };
        issues.push(Issue::new(
            vec![
                ("code", J::S("unrecognized_keys".into())),
                (
                    "keys",
                    J::A(unknown.iter().map(|k| J::S((*k).clone())).collect()),
                ),
            ],
            format!("Unrecognized {noun}: {}", quoted.join(", ")),
            // zod 4.6.5 的 unrecognized_keys 带 `continue: true`：不中断对象级校验，
            // 联合类型中只多出未知键的分支仍算未中断（原先按中断处理，分支选择与 zod 不同）。
            false,
        ));
    }
    (Value::Object(out), issues)
}

/// zod `handleUnionResults`: the first clean branch wins; a single branch that
/// failed only checks reports its own issues; otherwise one `invalid_union`.
fn union(value: Option<&Value>, options: &[Schema]) -> Parsed {
    let results: Vec<Parsed> = options.iter().map(|o| run(o, value)).collect();
    if let Some(clean) = results.iter().find(|(_, issues)| issues.is_empty()) {
        return clean.clone();
    }
    let open: Vec<&Parsed> = results.iter().filter(|(_, i)| !aborted(i)).collect();
    if let [single] = open.as_slice() {
        return (*single).clone();
    }
    let errors = results
        .iter()
        .map(|(_, issues)| J::A(issues.iter().map(Issue::json).collect()))
        .collect();
    fail(Issue::new(
        vec![
            ("code", J::S("invalid_union".into())),
            ("errors", J::A(errors)),
        ],
        "Invalid input".into(),
        true,
    ))
}

fn discriminated(
    value: Option<&Value>,
    key: &'static str,
    options: &[(&'static str, Schema)],
) -> Parsed {
    let Some(Value::Object(input)) = value else {
        let mut issue = invalid_type("object", value);
        issue.head.reverse();
        return fail(issue);
    };
    let tag = input.get(key).and_then(Value::as_str);
    if let Some((_, schema)) = options.iter().find(|(name, _)| Some(*name) == tag) {
        return run(schema, value);
    }
    let names = options
        .iter()
        .map(|(n, _)| format!("'{n}'"))
        .collect::<Vec<_>>();
    let issue = Issue::new(
        vec![
            ("code", J::S("invalid_union".into())),
            ("errors", J::A(vec![])),
            ("note", J::S("No matching discriminator".into())),
            ("discriminator", J::S(key.into())),
            (
                "options",
                J::A(options.iter().map(|(n, _)| J::S((*n).into())).collect()),
            ),
        ],
        format!(
            "Invalid discriminator value. Expected {}",
            names.join(" | ")
        ),
        true,
    );
    fail(issue.prefixed(J::S(key.into())))
}

/// zod `$ZodRecord` with a non-enumerable key schema: every key is checked
/// (a failing key is an aborting `invalid_key`), then its value.
fn record(value: Option<&Value>, key: &Schema, item: Option<&Schema>) -> Parsed {
    let Some(Value::Object(input)) = value else {
        return fail(invalid_type("record", value));
    };
    let mut out = Map::new();
    let mut issues = vec![];
    // serde_json 的对象按键排序；多个键的检查顺序可能与 JS 插入顺序不同（已知差异）。
    for (name, field) in input {
        let (_, found) = run(key, Some(&Value::String(name.clone())));
        if !found.is_empty() {
            let nested = found.iter().map(Issue::json).collect();
            let issue = Issue::new(
                vec![
                    ("code", J::S("invalid_key".into())),
                    ("origin", J::S("record".into())),
                    ("issues", J::A(nested)),
                ],
                "Invalid key in record".into(),
                true,
            );
            issues.push(issue.prefixed(J::S(name.clone())));
            continue;
        }
        let parsed = match item {
            Some(schema) => {
                let (parsed, found) = run(schema, Some(field));
                issues.extend(prefix(found, J::S(name.clone())));
                parsed
            }
            None => field.clone(),
        };
        out.insert(name.clone(), parsed);
    }
    (Value::Object(out), issues)
}
