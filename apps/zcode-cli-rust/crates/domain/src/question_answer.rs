use super::question::{QuestionAnswer, QuestionInput};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Answer {
    #[serde(default, deserialize_with = "super::question::optional")]
    option_id: Option<String>,
    #[serde(default, deserialize_with = "super::question::optional")]
    free_text: Option<String>,
    #[serde(default, deserialize_with = "super::question::optional")]
    action: Option<String>,
    #[serde(default, deserialize_with = "super::question::optional")]
    content: Option<serde_json::Map<String, Value>>,
}
impl QuestionInput {
    pub fn answer(&self, value: Value) -> Result<QuestionAnswer> {
        let answer: Answer = serde_json::from_value(value)?;
        let text = answer.free_text.as_deref().unwrap_or("").trim();
        let action = answer.action.as_deref().unwrap_or({
            if !text.is_empty()
                || matches!(
                    answer.option_id.as_deref(),
                    Some("allowOnce" | "allowAlways")
                )
            {
                "accept"
            } else {
                "decline"
            }
        });
        ensure!(
            matches!(action, "accept" | "decline" | "cancel"),
            "Invalid answer action"
        );
        if action != "accept" {
            return Ok(QuestionAnswer {
                content: format!(
                    "AskUserQuestion was {}",
                    if action == "cancel" {
                        "cancelled"
                    } else {
                        "declined"
                    }
                ),
                data: Value::Null,
                failed: true,
            });
        }
        let content = if answer.action.is_some() {
            Value::Object(answer.content.unwrap_or_default())
        } else if !text.is_empty() {
            json!({"answer":text})
        } else {
            json!({})
        };
        let mut answers = serde_json::Map::new();
        for (i, q) in self.questions.iter().enumerate() {
            let raw = content["answers"]
                .get(&q.question)
                .filter(|v| !v.is_null())
                .or_else(|| content.get(format!("answer_{i}")).filter(|v| !v.is_null()))
                .or_else(|| {
                    (self.questions.len() == 1)
                        .then(|| content.get("answer"))
                        .flatten()
                });
            if let Some(value) = raw.and_then(normalize) {
                answers.insert(q.question.clone(), value.into());
            }
        }
        if content["answers"].as_object().is_some_and(|a| a.is_empty()) {
            answers.clear();
        }
        if (answers.is_empty() && !content["answers"].as_object().is_some_and(|a| a.is_empty()))
            || answers
                .values()
                .any(|v| v.as_str().unwrap().trim().is_empty())
        {
            return Ok(QuestionAnswer {
                content: "AskUserQuestion requires user answers before execution".into(),
                data: Value::Null,
                failed: true,
            });
        }
        let annotations = content["annotations"]
            .as_object()
            .map(|a| {
                a.iter()
                    .filter_map(|(q, v)| {
                        let mut normalized = serde_json::Map::new();
                        for k in ["preview", "notes"] {
                            if let Some(text) = v[k].as_str() {
                                normalized.insert(k.into(), text.into());
                            }
                        }
                        (!normalized.is_empty()).then(|| (q.clone(), Value::Object(normalized)))
                    })
                    .collect::<serde_json::Map<_, _>>()
            })
            .unwrap_or_default();
        let mut data = json!({"questions":self.questions,"answers":answers});
        if !annotations.is_empty() {
            data["annotations"] = annotations.into();
        }
        let mut content = format_output(&data)?;
        // TS 的模型输出预算为 100000 bytes；不在 UTF-8 中间截断，也不把预览复制进全局状态。
        if content.len() > 100_000 {
            let mut end = 100_000;
            while !content.is_char_boundary(end) {
                end -= 1;
            }
            content.truncate(end);
        }
        Ok(QuestionAnswer {
            content,
            data,
            failed: false,
        })
    }
}
fn normalize(value: &Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        let s = s.trim();
        return (!s.is_empty()).then(|| s.into());
    }
    value.as_array().map(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ")
    })
}
fn format_output(data: &Value) -> Result<String> {
    let answers = data["answers"].as_object().context("Missing answers")?;
    if answers.is_empty() {
        return Ok("The user did not provide answers to these questions. Continue using your best judgment; do not treat this as a rejection or invent a user preference.".into());
    }
    let mut parts = vec![];
    let mut unanswered = 0;
    for q in data["questions"].as_array().context("Missing questions")? {
        let question = q["question"].as_str().unwrap();
        let Some(answer) = answers.get(question).and_then(Value::as_str) else {
            unanswered += 1;
            continue;
        };
        let mut part = format!("\"{question}\"=\"{answer}\"");
        let annotation = &data["annotations"][question];
        if let Some(p) = annotation["preview"].as_str().filter(|s| !s.is_empty()) {
            part.push_str(&format!(" selected preview:\n{p}"));
        }
        if let Some(n) = annotation["notes"].as_str().filter(|s| !s.is_empty()) {
            part.push_str(&format!(" user notes: {n}"));
        }
        parts.push(part);
    }
    let answers = parts.join(", ");
    Ok(if unanswered > 0 {
        format!(
            "The user answered some questions and skipped {unanswered}. Provided answers: {answers}. Continue with the provided answers and use your best judgment for the unanswered questions; do not invent user preferences."
        )
    } else {
        format!(
            "User has answered your questions: {answers}. You can now continue with the user's answers in mind."
        )
    })
}
