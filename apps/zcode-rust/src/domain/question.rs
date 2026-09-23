use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::OnceLock};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionItem {
    pub label: String,
    pub description: String,
    #[serde(
        default,
        deserialize_with = "optional",
        skip_serializing_if = "Option::is_none"
    )]
    pub preview: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Question {
    pub question: String,
    pub header: String,
    pub options: Vec<OptionItem>,
    #[serde(default)]
    pub multi_select: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Annotation {
    #[serde(
        default,
        deserialize_with = "optional",
        skip_serializing_if = "Option::is_none"
    )]
    pub preview: Option<String>,
    #[serde(
        default,
        deserialize_with = "optional",
        skip_serializing_if = "Option::is_none"
    )]
    pub notes: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    #[serde(
        default,
        deserialize_with = "optional",
        skip_serializing_if = "Option::is_none"
    )]
    pub source: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionInput {
    pub questions: Vec<Question>,
    #[serde(
        default,
        deserialize_with = "optional",
        skip_serializing_if = "Option::is_none"
    )]
    pub answers: Option<BTreeMap<String, String>>,
    #[serde(
        default,
        deserialize_with = "optional",
        skip_serializing_if = "Option::is_none"
    )]
    pub annotations: Option<BTreeMap<String, Annotation>>,
    #[serde(
        default,
        deserialize_with = "optional",
        skip_serializing_if = "Option::is_none"
    )]
    pub metadata: Option<Metadata>,
}
impl QuestionInput {
    pub fn parse(value: Value) -> Result<Self> {
        let input: Self = serde_json::from_value(value)?;
        ensure!(
            (1..=4).contains(&input.questions.len()),
            "Expected 1-4 questions"
        );
        let mut questions = std::collections::BTreeSet::new();
        for q in &input.questions {
            ensure!(
                questions.insert(&q.question),
                "Question texts must be unique"
            );
            ensure!((2..=4).contains(&q.options.len()), "Expected 2-4 options");
            let mut labels = std::collections::BTreeSet::new();
            for option in &q.options {
                ensure!(labels.insert(&option.label), "Option labels must be unique");
                ensure!(
                    !option.label.trim().eq_ignore_ascii_case("other"),
                    "Do not include an Other option"
                );
                if let Some(preview) = &option.preview {
                    validate_preview(preview)?;
                }
            }
        }
        Ok(input)
    }
    pub fn payload(&self, call_id: &str) -> Value {
        let questions = self.questions.iter().map(|q| json!({"question":q.question,"header":q.header,"multiSelect":q.multi_select,"options":q.options.iter().map(|o|{
            let mut option = json!({"value":o.label,"label":o.label,"description":o.description});
            if let Some(preview)=&o.preview { option["preview"]=preview.clone().into(); }
            option
        }).collect::<Vec<_>>()})).collect::<Vec<_>>();
        json!({"kind":"userInput","prompt":"AskUserQuestion pauses execution to collect answers from the user","freeText":true,"toolCallId":call_id,"toolName":"AskUserQuestion","input":self,"schema":{"toolName":"AskUserQuestion"},"questions":questions})
    }
}
fn validate_preview(preview: &str) -> Result<()> {
    static PATTERNS: OnceLock<[regex::Regex; 4]> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            r"(?i)<!doctype\b|<!--|</?\s*[a-z][a-z0-9:-]*(?:\s[^<>]*)?>",
            r"(?i)<!doctype\b|</?\s*(?:html|body)\b",
            r"(?i)</?\s*(?:script|style)\b",
            r"(?i)</?\s*[a-z][a-z0-9:-]*(?:\s[^<>]*)?>",
        ]
        .map(|p| regex::Regex::new(p).unwrap())
    });
    if patterns[0].is_match(preview) {
        ensure!(
            !patterns[1].is_match(preview),
            "HTML preview must be a fragment without html, body, or doctype"
        );
        ensure!(
            !patterns[2].is_match(preview),
            "HTML preview cannot contain script or style tags"
        );
        ensure!(
            patterns[3].is_match(preview),
            "HTML preview must contain an HTML tag"
        );
    }
    Ok(())
}

#[derive(Clone)]
pub struct QuestionAnswer {
    pub content: String,
    pub data: Value,
    pub failed: bool,
}

// Zod optional 允许缺省，但不接受 JSON null；serde Option 默认会吞掉 null，需显式保留差分。
pub(super) fn optional<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> std::result::Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}
