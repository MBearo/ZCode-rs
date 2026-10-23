//! 会话词组的词表（docs/specs/rust-cli/tools-more.md §11.6）：plan slug 的三张表，存在 `prompts/slug_words.json`。

use std::sync::OnceLock;

use rust_cli_base::include_lf;
use serde::Deserialize;

/// 形容词、动词、名词三张表（按这个顺序拼成 `形容词-动词-名词`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct SlugWords {
    pub adjectives: Vec<String>,
    pub verbs: Vec<String>,
    pub nouns: Vec<String>,
}

const SLUG_WORDS: &str = include_lf!("../../../prompts/slug_words.json");

/// 词表；数据文件解析失败时三张表都为空（单测保证能解析）。
pub fn slug_words() -> &'static SlugWords {
    static WORDS: OnceLock<SlugWords> = OnceLock::new();
    WORDS.get_or_init(|| serde_json::from_str(SLUG_WORDS).unwrap_or_default())
}
