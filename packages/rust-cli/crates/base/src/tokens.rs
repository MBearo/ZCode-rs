//! 粗估 token 的每 token 字符数（docs/specs/rust-cli/constants.md §2）：字符数按 UTF-16 码元（JS 的 `length`）。
//! 以前 prompts、kernel-api、ext、runtime、tools 各写 4 / 3 / 2（F122 收拢）。

/// 缺省：文字每 4 个字符一个 token（压缩的预算、`/context`、MCP 结果的上限、读文件的估算）。
pub const CHARS_PER_TOKEN: u64 = 4;
/// JSON 类的内容（工具定义、`.json` 文件）每 2 个字符一个 token。
pub const JSON_CHARS_PER_TOKEN: u64 = 2;
/// 不在旧模型清单里的模型每 3 个字符一个 token（prompts `models::chars_per_token`）。
pub const NEWER_MODEL_CHARS_PER_TOKEN: u64 = 3;

/// JS 的 `Math.round(units / chars_per_token)`（`units` 是 UTF-16 码元数，非负时四舍五入）。
pub const fn estimate(units: u64, chars_per_token: u64) -> u64 {
    (units * 2 + chars_per_token) / (chars_per_token * 2)
}

#[cfg(test)]
#[path = "tokens_tests.rs"]
mod tests;
