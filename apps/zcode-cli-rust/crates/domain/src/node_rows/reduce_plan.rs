//! Tool facts the tool reductions derive: official CUA app identities (Node
//! `cua-app-snapshot.ts`) and TodoWrite plans (Node `tool-plan-adapter.ts`
//! and `ProductProjection.todoPlanDeltas`).
use super::events::Event;
use super::projection::{Delta, Projection};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::OnceLock;

const OFFICIAL_CUA_PREFIXES: [&str; 2] = [
    "mcp__computer-use__",
    "mcp__plugin_zcode-cua_computer-use__",
];

/// Node `readOfficialCuaAction`.
pub fn cua_action(tool: &str) -> Option<&str> {
    OFFICIAL_CUA_PREFIXES
        .iter()
        .find_map(|prefix| tool.strip_prefix(prefix))
}

/// Node `parseJson`: the JSON before a "Structured content:" trailer.
fn parse_json(value: &str) -> Option<Value> {
    let candidate = value.split("\n\nStructured content:").next()?.trim();
    if candidate.is_empty() {
        return None;
    }
    serde_json::from_str(candidate).ok()
}

fn app_ref(value: &Value) -> Option<Value> {
    match value {
        Value::String(raw) => parse_json(raw).filter(Value::is_object),
        Value::Object(_) => Some(value.clone()),
        _ => None,
    }
}

fn app_candidates(input: &Value) -> [&Value; 3] {
    [
        &input["app_ref"],
        &input["app"],
        &input["target"]["app_ref"],
    ]
}

/// Node `resolveCuaAppIdentity`: by pid, else by a bundle id only one app has.
pub fn resolve_cua_app(input: &Value, snapshot: &HashMap<i64, Value>) -> Option<Value> {
    if !input.is_object() {
        return None;
    }
    let pid = app_candidates(input).into_iter().find_map(|candidate| {
        let pid = app_ref(candidate)?["pid"].as_f64()?;
        (pid.fract() == 0.0 && pid > 0.0).then_some(pid as i64)
    });
    if let Some(pid) = pid {
        return snapshot.get(&pid).cloned();
    }
    let bundle = app_candidates(input).into_iter().find_map(|candidate| {
        let bundle = app_ref(candidate)?["bundle_id"].as_str()?.trim().to_owned();
        (!bundle.is_empty()).then_some(bundle)
    })?;
    let mut matches = snapshot
        .values()
        .filter(|app| app["bundleId"] == bundle.as_str());
    let first = matches.next()?;
    matches.next().is_none().then(|| first.clone())
}

/// Node `unwrapApps`.
fn unwrap_apps(value: &Value) -> Option<Vec<Value>> {
    match value {
        Value::Array(items) => Some(items.clone()),
        Value::Object(record) => match record.get("apps") {
            Some(Value::Array(apps)) => Some(apps.clone()),
            _ => match record.get("result") {
                Some(Value::String(raw)) => unwrap_apps(&parse_json(raw).unwrap_or(Value::Null)),
                Some(other) => unwrap_apps(other),
                None => None,
            },
        },
        _ => None,
    }
}

/// Node `parseListAppsSnapshot`.
pub fn parse_list_apps(content: &str, display: &Value) -> Option<HashMap<i64, Value>> {
    let structured = match (&display["kind"], display["structuredContent"].as_str()) {
        (kind, Some(raw)) if kind == "cua" => parse_json(raw),
        _ => None,
    };
    let rows = structured
        .as_ref()
        .and_then(unwrap_apps)
        .or_else(|| unwrap_apps(&parse_json(content)?))?;
    let mut snapshot = HashMap::new();
    for record in rows.iter().filter(|r| r.is_object()) {
        let Some(pid) = record["pid"]
            .as_f64()
            .filter(|p| p.fract() == 0.0 && *p > 0.0)
        else {
            continue;
        };
        let name = record["name"].as_str().map(str::trim).unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let mut app = json!({"pid": pid as i64, "name": name});
        if let Some(bundle) = record["bundle_id"]
            .as_str()
            .map(str::trim)
            .filter(|b| !b.is_empty())
        {
            app["bundleId"] = bundle.into();
        }
        snapshot.insert(pid as i64, app);
    }
    Some(snapshot)
}

fn todo_tool_regex() -> &'static regress::Regex {
    static REGEX: OnceLock<regress::Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        regress::Regex::with_flags(
            r"(?:^|[_\s-])(?:todo[_\s-]*(?:read|write)|update[_\s-]*plan)(?:$|[_\s-])",
            "i",
        )
        .expect("valid regex")
    })
}

/// Node `isTodoPlanToolName` over the `[title, kind]` fingerprint.
fn is_todo_tool(name: &Value) -> bool {
    let name = name.as_str().unwrap_or("");
    let fingerprint = if name.is_empty() {
        String::new()
    } else {
        format!("{name} {name}")
    };
    todo_tool_regex().find(fingerprint.trim()).is_some()
}

fn read_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Node `parsePlanStep`.
fn plan_step(value: &Value, index: usize) -> Option<(String, String, &'static str)> {
    if let Value::String(title) = value {
        let title = title.trim();
        let status = if index == 0 { "in_progress" } else { "pending" };
        return (!title.is_empty()).then(|| (title.to_owned(), title.to_owned(), status));
    }
    value.as_object()?;
    let title = ["content", "step", "title", "text", "activeForm"]
        .iter()
        .find_map(|key| read_string(&value[*key]))?;
    let status = read_string(&value["status"])?
        .replace('-', "_")
        .to_lowercase();
    let status = match status.as_str() {
        "pending" => "pending",
        "in_progress" => "in_progress",
        "completed" => "completed",
        _ => return None,
    };
    Some((
        read_string(&value["id"]).unwrap_or_else(|| title.clone()),
        title,
        status,
    ))
}

/// Node `extractPlanStepsFromValue`: every item must parse.
fn steps_of(value: &Value) -> Option<Vec<(String, String, &'static str)>> {
    let parsed;
    let value = match value {
        Value::String(raw) => {
            parsed = serde_json::from_str::<Value>(raw).ok()?;
            &parsed
        }
        other => other,
    };
    let collection = ["todos", "plan", "steps", "items"]
        .iter()
        .find_map(|key| value.as_object()?.get(*key)?.as_array())?;
    if collection.is_empty() {
        return None;
    }
    let steps: Vec<_> = collection
        .iter()
        .enumerate()
        .filter_map(|(index, item)| plan_step(item, index))
        .collect();
    (steps.len() == collection.len()).then_some(steps)
}

type Steps = Option<Vec<(String, String, &'static str)>>;

/// Node `extractPlanStepsFromToolInput`.
pub fn steps_from_input(name: &Value, input: Option<&Value>) -> Steps {
    if !is_todo_tool(name) {
        return None;
    }
    steps_of(input.unwrap_or(&Value::Null))
}

/// Node `extractPlanStepsFromToolOutput`.
pub fn steps_from_output(name: &Value, output: &Value) -> Steps {
    if !is_todo_tool(name) {
        return None;
    }
    let mut candidates = vec![output.clone()];
    if let Some(parsed) = output
        .as_str()
        .and_then(|raw| serde_json::from_str(raw).ok())
    {
        candidates.push(parsed);
    }
    candidates
        .into_iter()
        .find_map(|candidate| steps_of(&candidate))
}

impl Projection {
    /// Node `todoPlanDeltas`: the live plan, and the current goal iteration's.
    pub fn todo_plan(&self, event: &Event, steps: Steps) -> Vec<Delta> {
        let Some(steps) = steps else {
            return Vec::new();
        };
        let items: Vec<Value> = steps
            .into_iter()
            .enumerate()
            .map(|(index, (id, title, status))| {
                let id = if id.is_empty() {
                    format!("todo-{}", index + 1)
                } else {
                    id
                };
                let status = match status {
                    "in_progress" => "inProgress",
                    "completed" => "completed",
                    _ => "pending",
                };
                json!({"id": id, "content": title, "status": status})
            })
            .collect();
        let plan = json!({"items": items, "updatedAt": event.at});
        let mut patch = Map::new();
        let goal = &self.state["goal"];
        if goal.is_object() {
            let current = goal["iteration"].as_f64().unwrap_or(0.0);
            let settled = matches!(
                goal["status"].as_str(),
                Some("verifying" | "verified" | "failed")
            );
            let iteration = if settled {
                current.max(1.0)
            } else {
                (current + 1.0).max(1.0)
            };
            let mut iterations: Vec<Value> = goal["iterations"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|entry| entry["iteration"].as_f64() != Some(iteration))
                .cloned()
                .collect();
            iterations.push(json!({"iteration": super::events::num(iteration), "items": items, "updatedAt": event.at}));
            iterations.sort_by(|a, b| {
                let key = |v: &Value| v["iteration"].as_f64().unwrap_or(0.0);
                key(a)
                    .partial_cmp(&key(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let mut goal = goal.clone();
            goal["iterations"] = iterations.into();
            patch.insert("goal".into(), goal);
        }
        patch.insert("plan".into(), plan);
        vec![Delta::State(patch)]
    }
}
