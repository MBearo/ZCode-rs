//! The subset of zod 4 parsing used by legacy protocol params, reproducing its
//! issue shapes, key order and messages (Node `parseParams` sends them to the
//! Host verbatim).
//!
//! Issue paths are relative to the node that raised them; parents prefix their
//! key, and union branches keep branch-relative paths inside `errors`, as zod does.
use serde_json::Value;

/// JSON with insertion-ordered objects, printed like `JSON.stringify(v, null, 2)`.
#[derive(Clone, Debug)]
pub enum J {
    S(String),
    N(i64),
    A(Vec<J>),
    O(Vec<(&'static str, J)>),
    V(Value),
}

impl J {
    fn write(&self, out: &mut String, indent: usize) {
        let pad = |n: usize| "  ".repeat(n);
        match self {
            J::S(s) => out.push_str(&Value::String(s.clone()).to_string()),
            J::N(n) => out.push_str(&n.to_string()),
            J::V(v) => out.push_str(&v.to_string()),
            J::A(items) if items.is_empty() => out.push_str("[]"),
            J::A(items) => {
                out.push_str("[\n");
                for (i, item) in items.iter().enumerate() {
                    out.push_str(&pad(indent + 1));
                    item.write(out, indent + 1);
                    out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
                }
                out.push_str(&pad(indent));
                out.push(']');
            }
            J::O(fields) => {
                out.push_str("{\n");
                for (i, (key, value)) in fields.iter().enumerate() {
                    out.push_str(&pad(indent + 1));
                    out.push_str(&Value::String((*key).into()).to_string());
                    out.push_str(": ");
                    value.write(out, indent + 1);
                    out.push_str(if i + 1 < fields.len() { ",\n" } else { "\n" });
                }
                out.push_str(&pad(indent));
                out.push('}');
            }
        }
    }

    pub fn pretty(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out
    }
}

#[derive(Clone, Debug)]
pub struct Issue {
    /// Fields before `path`, in zod's order.
    head: Vec<(&'static str, J)>,
    path: Vec<J>,
    message: String,
    /// zod `continue !== true`: type errors abort, checks (lengths, formats) do not.
    aborts: bool,
    /// `ctx.addIssue` issues keep the caller's key order: `code, message, path`.
    custom: bool,
}

impl Issue {
    fn new(head: Vec<(&'static str, J)>, message: String, aborts: bool) -> Self {
        Self {
            head,
            path: vec![],
            message,
            aborts,
            custom: false,
        }
    }

    /// A `superRefine` issue (`code: "custom"`, never aborting).
    pub fn custom(path: &[&'static str], message: &str) -> Self {
        Self {
            head: vec![("code", J::S("custom".into()))],
            path: path.iter().map(|p| J::S((*p).into())).collect(),
            message: message.into(),
            aborts: false,
            custom: true,
        }
    }

    fn prefixed(mut self, segment: J) -> Self {
        self.path.insert(0, segment);
        self
    }

    pub fn code(&self) -> &str {
        match self.field("code") {
            Some(J::S(code)) => code,
            _ => "",
        }
    }

    /// A head field (`expected`, `keys`, …).
    pub fn field(&self, key: &str) -> Option<&J> {
        self.head.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// Node tool-input path text: `a.b[0].c` (root is empty).
    pub fn path_text(&self) -> String {
        let mut text = String::new();
        for segment in &self.path {
            match segment {
                J::N(n) => text.push_str(&format!("[{n}]")),
                J::S(s) if text.is_empty() => text.push_str(s),
                J::S(s) => text.push_str(&format!(".{s}")),
                _ => {}
            }
        }
        text
    }

    pub fn json(&self) -> J {
        let mut fields = self.head.clone();
        let path = ("path", J::A(self.path.clone()));
        let message = ("message", J::S(self.message.clone()));
        if self.custom {
            fields.extend([message, path]);
        } else {
            fields.extend([path, message]);
        }
        J::O(fields)
    }

    /// Node `summarizeParamsError` entry: `path.join(".")` or `(root)`.
    fn summary(&self) -> String {
        let path = self
            .path
            .iter()
            .map(|p| match p {
                J::S(s) => s.clone(),
                J::N(n) => n.to_string(),
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join(".");
        let path = if path.is_empty() {
            "(root)".into()
        } else {
            path
        };
        format!("{path}: {}", self.message)
    }
}

/// Node `parseParams` failure: `(message, data)` of the `-32602` error.
pub fn protocol_error(issues: &[Issue]) -> (String, Value) {
    let mut detail = issues
        .iter()
        .take(5)
        .map(Issue::summary)
        .collect::<Vec<_>>()
        .join("; ");
    if issues.len() > 5 {
        detail.push_str(&format!(" (+{} more)", issues.len() - 5));
    }
    let message = if detail.is_empty() {
        "Invalid params".to_owned()
    } else {
        format!("Invalid params — {detail}")
    };
    let data = J::A(issues.iter().map(Issue::json).collect()).pretty();
    (
        message,
        serde_json::json!({"name":"ZodError","message":data}),
    )
}

pub enum Format {
    Hex64,
    Url,
}

pub enum Schema {
    String {
        trim: bool,
        min: Option<usize>,
        max: Option<usize>,
        format: Option<Format>,
    },
    Bool,
    /// `z.number().int()` with an optional lower bound `(minimum, inclusive)`
    /// and an optional inclusive upper bound, checked in that order.
    Int(Option<(i64, bool)>, Option<i64>),
    Enum(&'static [&'static str]),
    Literal(Value),
    Array(Box<Schema>, Option<usize>),
    /// Strict object: `(key, schema, optional)`.
    Object(Vec<(&'static str, Schema, bool)>),
    Union(Vec<Schema>),
    Discriminated(&'static str, Vec<(&'static str, Schema)>),
    /// `z.record(key, value)`; no value schema is `z.unknown()`.
    Record(Box<Schema>, Option<Box<Schema>>),
    /// `.superRefine(check)`: `check` sees the parsed value and runs only when
    /// no aborting issue was raised.
    Refined(Box<Schema>, fn(&Value) -> Vec<Issue>),
}

/// `z.string().trim().min(1)`.
pub fn non_empty() -> Schema {
    Schema::String {
        trim: true,
        min: Some(1),
        max: None,
        format: None,
    }
}

pub fn string() -> Schema {
    Schema::String {
        trim: false,
        min: None,
        max: None,
        format: None,
    }
}

/// `z.string().min(min)` (not trimmed).
pub fn string_min(min: usize) -> Schema {
    Schema::String {
        trim: false,
        min: Some(min),
        max: None,
        format: None,
    }
}

pub fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| {
        matches!(
            c,
            '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
                ..='\u{200A}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{202F}'
                    | '\u{205F}'
                    | '\u{3000}'
                    | '\u{FEFF}'
        )
    })
}

fn received(value: Option<&Value>) -> &'static str {
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

fn invalid_type(expected: &'static str, value: Option<&Value>) -> Issue {
    Issue::new(
        vec![
            ("expected", J::S(expected.into())),
            ("code", J::S("invalid_type".into())),
        ],
        format!(
            "Invalid input: expected {expected}, received {}",
            received(value)
        ),
        true,
    )
}

fn too_small(origin: &'static str, minimum: i64, inclusive: bool, message: String) -> Issue {
    Issue::new(
        vec![
            ("origin", J::S(origin.into())),
            ("code", J::S("too_small".into())),
            ("minimum", J::N(minimum)),
            ("inclusive", J::V(Value::Bool(inclusive))),
        ],
        message,
        false,
    )
}

/// `.max()` of numbers and strings (always inclusive).
fn too_big(origin: &'static str, maximum: i64, message: String) -> Issue {
    Issue::new(
        vec![
            ("origin", J::S(origin.into())),
            ("code", J::S("too_big".into())),
            ("maximum", J::N(maximum)),
            ("inclusive", J::V(Value::Bool(true))),
        ],
        message,
        false,
    )
}

pub type Parsed = (Value, Vec<Issue>);

pub fn aborted(issues: &[Issue]) -> bool {
    issues.iter().any(|i| i.aborts)
}

impl Schema {
    /// Parses `value` (`None` is JS `undefined`), returning the output value
    /// (strings trimmed where the schema says so) and the issues.
    pub fn run(&self, value: Option<&Value>) -> Parsed {
        run::run(self, value)
    }
}

#[path = "zod_run.rs"]
mod run;
