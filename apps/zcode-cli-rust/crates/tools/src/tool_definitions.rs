//! Built-in tool definitions sent to the model (descriptions and schemas are
//! generated from Node by `scripts/generate-zcode-cli-rust-tool-schemas.mjs`).
use serde_json::{Value, json};

pub(super) fn definitions() -> Vec<Value> {
    let schemas: Value =
        serde_json::from_str(include_str!("tool_schemas.json")).expect("validated tool schemas");
    let mut definitions: Vec<Value> = [
        ("Read","Read a text file with 1-based numbered lines. Use offset and limit for large files."),
        ("Write","Create or overwrite a text file. Read existing files fully before overwriting."),
        ("Edit","Replace a unique exact string, or every occurrence with replace_all. Read the file first."),
        ("Glob","Find files by glob pattern, sorted by modification time. Returns at most 100 matches."),
        ("Grep","Search text with regex, glob/type filters, context, multiline and paginated output."),
        ("Bash","Execute a shell command in the workspace. run_in_background returns a task ID and output path. Use TaskOutput or Read for output; TaskStop stops the process tree."),
        ("TaskOutput","Retrieve a session-owned background shell task's output. block waits up to timeout milliseconds."),
        ("TaskStop","Stop a session-owned background shell task and wait for its process tree to exit."),
    ].into_iter().map(|(name,description)|json!({"type":"function","function":{"name":name,"description":description,"parameters":schemas[name]}})).collect();
    let description: String = serde_json::from_str(include_str!("skill_description.json"))
        .expect("validated Skill description");
    definitions.push(json!({"type":"function","function":{"name":"Skill","description":description,"parameters":schemas["Skill"]}}));
    let description: String = serde_json::from_str(include_str!("question_description.json"))
        .expect("validated question description");
    definitions.push(json!({"type":"function","function":{"name":"AskUserQuestion","description":description,"parameters":schemas["AskUserQuestion"]}}));
    let descriptions: Value = serde_json::from_str(include_str!("todo_descriptions.json"))
        .expect("validated todo descriptions");
    for name in ["TodoRead", "TodoWrite"] {
        definitions.push(json!({"type":"function","function":{"name":name,"description":descriptions[name],"parameters":schemas[name]}}));
    }
    let descriptions: Value =
        serde_json::from_str(include_str!("agent_descriptions.json")).expect("agent descriptions");
    for name in ["Agent", "SendMessage"] {
        definitions.push(json!({"type":"function","function":{"name":name,"description":descriptions[name],"parameters":schemas[name]}}));
    }
    let descriptions: Value =
        serde_json::from_str(include_str!("web_descriptions.json")).expect("web tool descriptions");
    definitions.push(json!({"type":"function","function":{"name":"WebFetch","description":descriptions["WebFetch"],"parameters":schemas["WebFetch"]}}));
    // Node 的描述 getter 每次按本地时间给出当前月份（英文月份名）。
    let month = chrono::Local::now().format("%B %Y").to_string();
    let template = descriptions["WebSearch"].as_str().unwrap_or_default();
    let description = crate::domain::web::search::description(template, &month);
    definitions.push(json!({"type":"function","function":{"name":"WebSearch","description":description,"parameters":schemas["WebSearch"]}}));
    definitions.extend(crate::domain::plan_mode::definitions());
    definitions
}

/// Node `getTools(model)`: a model with PDF input gets Read's `pages` variant
/// (schema and description follow the model).
pub(super) fn for_model(definitions: &mut [Value], input_format: &Value) {
    if input_format["supportsPdf"] != true {
        return;
    }
    let schemas: Value =
        serde_json::from_str(include_str!("tool_schemas.json")).expect("validated tool schemas");
    if let Some(read) = definitions
        .iter_mut()
        .find(|d| d["function"]["name"] == "Read")
    {
        read["function"]["parameters"] = schemas["ReadPdf"].clone();
        let description = read["function"]["description"].as_str().unwrap_or("");
        read["function"]["description"] = format!(
            "{description}\n{}",
            crate::domain::read_pdf::PAGES_DESCRIPTION
        )
        .into();
    }
}
