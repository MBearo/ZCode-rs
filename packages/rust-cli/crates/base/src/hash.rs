//! FNV-1a 64 位哈希（docs/specs/rust-cli/constants.md §2）：跨平台、跨版本稳定（不用 std 的随机种子哈希），用于
//! 从 ID 派生短 ID、选词组。以前 hooks、tools（两处）、tui 各写一份两个常数（F122 收拢）。

use crate::js::radix::{DIGITS, RADIX};

/// FNV-1a 64 位的初始值。
pub const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64 位的乘数。
pub const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
/// [`short_id`] 的字符数。
pub const SHORT_ID_LEN: usize = 8;

/// 字节串的 FNV-1a 64 位哈希。
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

/// `text` 的 FNV-1a 写成 8 位 36 进制（低位先出，小写）：同一个输入总是同一个 ID（Bash 与 hook 命令的任务 ID）。
pub fn short_id(text: &str) -> String {
    let mut hash = fnv1a_64(text.as_bytes());
    let mut id = String::with_capacity(SHORT_ID_LEN);
    for _ in 0..SHORT_ID_LEN {
        id.push(char::from(DIGITS[(hash % RADIX) as usize]));
        hash /= RADIX;
    }
    id
}

#[cfg(test)]
#[path = "hash_tests.rs"]
mod tests;
