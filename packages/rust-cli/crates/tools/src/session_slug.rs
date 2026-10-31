//! 会话的词组（docs/specs/rust-cli/tools-more.md §11.6）：EnterWorktree 不给名字时的缺省名。
//!
//! 词组是会话的 plan slug：每个会话第一次用到时随机生成 `形容词-动词-名词`，按会话 ID
//! 缓存并写进 transcript，resume 时恢复，fork 与 `/clear` 换新的。这里让词组只由会话 ID 决定（唯一来源
//! 是会话侧的 ID），所以不用缓存、不用持久化，也就没有第二份状态：同一会话（含 resume、子代理经
//! `owner_session`）总是同一个词组，换了会话 ID 就是新词组，结果与随机生成、按会话缓存的规则一致。

use rust_cli_kernel_api::ids::SessionId;

/// 会话的词组 `形容词-动词-名词`：哈希依次对三张表的个数取余并整除，得到三个下标。
pub(crate) fn session_slug(session: &SessionId) -> String {
    let words = rust_cli_prompts::slug_words();
    // FNV-1a 64 位：跨平台、跨版本稳定（不用 std 的随机种子哈希）。
    let mut hash = rust_cli_base::hash::fnv1a_64(session.as_str().as_bytes());
    let mut pick = |list: &[String]| {
        let len = u64::try_from(list.len().max(1)).unwrap_or(1);
        let index = usize::try_from(hash % len).unwrap_or(0);
        hash /= len;
        list.get(index).map_or("", String::as_str).to_owned()
    };
    let adjective = pick(&words.adjectives);
    let verb = pick(&words.verbs);
    let noun = pick(&words.nouns);
    format!("{adjective}-{verb}-{noun}")
}

#[cfg(test)]
#[path = "session_slug_tests.rs"]
mod tests;
