//! Jupyter notebook 的解析、给模型看的渲染与单元格编辑（tools-files.md §1）。

use rust_cli_kernel_api::message::{ImageSource, ToolResultContent};
use serde_json::{Map, Value, json};

use crate::texts;

/// 单元格输出合计超过这么多字符时整体换成提示。
const OUTPUTS_LIMIT: usize = 10_000;
/// 单段输出文本的截断长度（`BASH_MAX_OUTPUT_LENGTH` 的默认值，与 Bash 的内联上限同一个值，F55）。
const OUTPUT_TEXT_LIMIT: usize = crate::bash_output::INLINE_LIMIT;

/// 解析并校验：顶层 `cells` 是对象数组，每个 `source` 是字符串或字符串数组。
pub(crate) fn parse(text: &str) -> Result<Value, &'static str> {
    let value: Value = serde_json::from_str(text).map_err(|_| texts::NOTEBOOK_BAD_JSON)?;
    let valid = value
        .get("cells")
        .and_then(Value::as_array)
        .is_some_and(|cells| {
            cells.iter().all(|cell| match cell.get("source") {
                Some(Value::String(_)) => true,
                Some(Value::Array(parts)) => parts.iter().all(Value::is_string),
                _ => false,
            } && cell.is_object())
        });
    if valid {
        Ok(value)
    } else {
        Err(texts::NOTEBOOK_INVALID)
    }
}

pub(crate) fn language(notebook: &Value) -> String {
    notebook
        .pointer("/metadata/language_info/name")
        .and_then(Value::as_str)
        .unwrap_or("python")
        .to_owned()
}

/// `source` 是数组时拼接。
pub(crate) fn source(cell: &Value) -> String {
    match cell.get("source") {
        Some(Value::Array(parts)) => parts.iter().filter_map(Value::as_str).collect(),
        Some(Value::String(text)) => text.clone(),
        _ => String::new(),
    }
}

fn joined_text(value: Option<&Value>) -> String {
    let raw = match value {
        Some(Value::Array(parts)) => parts.iter().filter_map(Value::as_str).collect(),
        Some(Value::String(text)) => text.clone(),
        _ => return String::new(),
    };
    // processOutputText → formatOutput：超长时保留前 N 个字符并注明截掉的行数。
    if raw.chars().count() <= OUTPUT_TEXT_LIMIT {
        return raw;
    }
    let head: String = raw.chars().take(OUTPUT_TEXT_LIMIT).collect();
    let rest = &raw[head.len()..];
    let lines = rest.matches('\n').count() + 1;
    format!("{head}\n\n... [{lines} lines truncated] ...")
}

fn image(data: &Map<String, Value>) -> Option<Value> {
    ["image/png", "image/jpeg"].iter().find_map(|media| {
        let raw = data.get(*media)?.as_str()?;
        let compact: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
        Some(json!({"image_data": compact, "media_type": media}))
    })
}

fn output(raw: &Value) -> Value {
    let kind = raw.get("output_type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "stream" => json!({"output_type": kind, "text": joined_text(raw.get("text"))}),
        "execute_result" | "display_data" => {
            let data = raw.get("data").and_then(Value::as_object);
            let mut out = json!({
                "output_type": kind,
                "text": joined_text(data.and_then(|data| data.get("text/plain"))),
            });
            if let Some(image) = data.and_then(image) {
                out["image"] = image;
            }
            out
        }
        "error" => {
            let name = raw.get("ename").and_then(Value::as_str).unwrap_or("");
            let value = raw.get("evalue").and_then(Value::as_str).unwrap_or("");
            let trace: Vec<&str> = raw
                .get("traceback")
                .and_then(Value::as_array)
                .map(|lines| lines.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let text = format!("{name}: {value}\n{}", trace.join("\n"));
            json!({"output_type": kind, "text": joined_text(Some(&Value::String(text)))})
        }
        _ => Value::Null,
    }
}

/// 一个单元格的模型视图。
fn cell_view(cell: &Value, index: usize, language: &str, bash: &str) -> Value {
    let kind = cell.get("cell_type").and_then(Value::as_str).unwrap_or("");
    let id = cell
        .get("id")
        .and_then(Value::as_str)
        .map_or_else(|| format!("cell-{index}"), str::to_owned);
    let mut view = Map::new();
    view.insert("cellType".to_owned(), json!(kind));
    view.insert("source".to_owned(), json!(source(cell)));
    if kind == "code"
        && let Some(count) = cell
            .get("execution_count")
            .filter(|count| count.as_f64().is_some_and(|value| value != 0.0))
    {
        view.insert("execution_count".to_owned(), count.clone());
    }
    view.insert("cell_id".to_owned(), json!(id));
    if kind == "code" {
        view.insert("language".to_owned(), json!(language));
    }
    let outputs = cell.get("outputs").and_then(Value::as_array);
    if kind == "code"
        && let Some(outputs) = outputs.filter(|outputs| !outputs.is_empty())
    {
        let mapped: Vec<Value> = outputs.iter().map(output).collect();
        let size: usize = mapped
            .iter()
            .map(|item| {
                item["text"].as_str().map_or(0, str::len)
                    + item["image"]["image_data"].as_str().map_or(0, str::len)
            })
            .sum();
        let shown = if size > OUTPUTS_LIMIT {
            vec![json!({
                "output_type": "stream",
                "text": format!("Outputs are too large to include. Use {bash} with: cat <notebook_path> | jq '.cells[{index}].outputs'"),
            })]
        } else {
            mapped
        };
        view.insert("outputs".to_owned(), Value::Array(shown));
    }
    Value::Object(view)
}

/// 全部单元格的模型视图；它的 JSON 也是 readFileState 里记的内容。
pub(crate) fn cells(notebook: &Value) -> Vec<Value> {
    let language = language(notebook);
    notebook["cells"]
        .as_array()
        .map(|cells| {
            cells
                .iter()
                .enumerate()
                .map(|(index, cell)| cell_view(cell, index, &language, "Bash"))
                .collect()
        })
        .unwrap_or_default()
}

/// tool_result 的内容块：相邻的文本块用换行合并。
pub(crate) fn render(cells: &[Value]) -> Vec<ToolResultContent> {
    let mut blocks: Vec<ToolResultContent> = Vec::new();
    let push_text = |blocks: &mut Vec<ToolResultContent>, text: String| match blocks.last_mut() {
        Some(ToolResultContent::Text { text: last }) => {
            last.push('\n');
            last.push_str(&text);
        }
        _ => blocks.push(ToolResultContent::Text { text }),
    };
    for cell in cells {
        let id = cell["cell_id"].as_str().unwrap_or("");
        let kind = cell["cellType"].as_str().unwrap_or("");
        let mut tags = String::new();
        if kind != "code" {
            tags.push_str(&format!("<cell_type>{kind}</cell_type>"));
        }
        let language = cell["language"].as_str().unwrap_or("python");
        if kind == "code" && language != "python" {
            tags.push_str(&format!("<language>{language}</language>"));
        }
        let source = cell["source"].as_str().unwrap_or("");
        push_text(
            &mut blocks,
            format!("<cell id=\"{id}\">{tags}{source}</cell id=\"{id}\">"),
        );
        for output in cell["outputs"].as_array().into_iter().flatten() {
            if let Some(text) = output["text"].as_str().filter(|text| !text.is_empty()) {
                push_text(&mut blocks, format!("\n{text}"));
            }
            if let (Some(data), Some(media)) = (
                output["image"]["image_data"].as_str(),
                output["image"]["media_type"].as_str(),
            ) {
                blocks.push(ToolResultContent::Image {
                    source: ImageSource::Base64 {
                        media_type: media.to_owned(),
                        data: data.to_owned(),
                    },
                });
            }
        }
    }
    blocks
}

/// `cell-N` 形式的 ID 对应的下标。
pub(crate) fn index_from_id(id: &str) -> Option<usize> {
    id.strip_prefix("cell-")
        .filter(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
        .and_then(|digits| digits.parse().ok())
}

/// nbformat ≥ 4.5 的单元格带 ID。
pub(crate) fn has_cell_ids(notebook: &Value) -> bool {
    let major = notebook["nbformat"].as_u64().unwrap_or(0);
    let minor = notebook["nbformat_minor"].as_u64().unwrap_or(0);
    major > 4 || (major == 4 && minor >= 5)
}

/// `JSON.stringify(notebook, null, 1)`。
pub(crate) fn serialize(notebook: &Value) -> String {
    let mut bytes = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut bytes, formatter);
    if serde::Serialize::serialize(notebook, &mut serializer).is_err() {
        return String::new();
    }
    String::from_utf8(bytes).unwrap_or_default()
}

#[cfg(test)]
#[path = "notebook_tests.rs"]
mod tests;
