//! The strict Node schemas the cold projection parses stored metadata with
//! (`schema/node-projection.json`, generated from the TS schemas by
//! `scripts/zcode-cli-rust-node-projection-schemas.mjs`). A parse is zod
//! `safeParse(...).data`: validation, then zod's output (unknown keys stripped,
//! defaults filled, declared keys first).
use crate::js_json::stringify;
use serde_json::{Map, Value};
use std::sync::OnceLock;
use zcode_cli_protocol::json_schema::Node;

struct Schemas {
    intent: Node,
    attribution: Node,
    workflow_launch: Node,
    workflow_notification: Node,
    tool_metadata: Node,
}

fn schemas() -> &'static Schemas {
    static SCHEMAS: OnceLock<Schemas> = OnceLock::new();
    SCHEMAS.get_or_init(|| {
        let raw: Value = serde_json::from_str(include_str!("../../schema/node-projection.json"))
            .expect("node-projection.json is valid JSON");
        let compile = |key: &str| {
            Node::compile(&raw[key]).expect("node projection schemas use the supported subset")
        };
        Schemas {
            intent: compile("conversationInputIntent"),
            attribution: compile("errorAttribution"),
            workflow_launch: compile("workflowLaunchMeta"),
            workflow_notification: compile("workflowNotificationMeta"),
            tool_metadata: compile("completedToolPartMetadata"),
        }
    })
}

fn parse(node: &Node, value: &Value) -> Option<Value> {
    node.validate(value, "").ok().map(|()| node.strip(value))
}

/// `conversationInputIntentSchema.safeParse`.
pub fn input_intent(value: &Value) -> Option<Value> {
    parse(&schemas().intent, value)
}

/// `errorAttributionSchema.safeParse`.
pub fn error_attribution(value: &Value) -> Option<Value> {
    parse(&schemas().attribution, value)
}

/// `workflowLaunchMetaSchema.safeParse`, including the `args` size refinement
/// JSON Schema cannot carry (serialized args at most 4096 UTF-16 units).
pub fn workflow_launch(value: &Value) -> Option<Value> {
    let args = &value["args"];
    if args.is_object() && stringify(args).encode_utf16().count() > 4096 {
        return None;
    }
    parse(&schemas().workflow_launch, value)
}

/// `workflowNotificationMetaSchema.safeParse`.
pub fn workflow_notification(value: &Value) -> Option<Value> {
    parse(&schemas().workflow_notification, value)
}

/// Node `parseCompletedToolPartMetadata`: scrub the withdrawn display fields
/// older sessions persisted, then parse strictly.
pub fn completed_tool_metadata(value: &Value) -> Option<Value> {
    let scrubbed = match value {
        Value::Object(map) if map.contains_key("display") => {
            let mut map = map.clone();
            let display = scrub_display(&map["display"]);
            map.insert("display".into(), display);
            Value::Object(map)
        }
        other => other.clone(),
    };
    parse(&schemas().tool_metadata, &scrubbed)
}

/// Node `scrubPersistedDisplay`.
fn scrub_display(display: &Value) -> Value {
    strip_provider_stop(strip_refined_names(display.clone()))
}

fn without(map: &Map<String, Value>, key: &str) -> Map<String, Value> {
    let mut map = map.clone();
    map.shift_remove(key);
    map
}

/// Node `stripProviderStopFromGetWorkflowRunError`.
fn strip_provider_stop(mut display: Value) -> Value {
    if display["kind"] != "get_workflow_run" {
        return display;
    }
    if let Some(error) = display["error"].as_object()
        && error.contains_key("providerStop")
    {
        display["error"] = Value::Object(without(error, "providerStop"));
    }
    display
}

/// Node `stripWithdrawnRefinedNames`.
fn strip_refined_names(mut display: Value) -> Value {
    if display["kind"] != "create_workflow" {
        return display;
    }
    let Some(graph) = display["causalityGraph"].as_object_mut() else {
        return display;
    };
    for (collection, key) in [
        ("lanes", "refinedName"),
        ("steps", "refinedLabel"),
        ("phases", "refinedName"),
    ] {
        if let Some(Value::Array(items)) = graph.get_mut(collection) {
            for item in items {
                if let Value::Object(map) = item {
                    map.shift_remove(key);
                }
            }
        }
    }
    display
}
