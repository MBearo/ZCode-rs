//! `Number.prototype.toString(36)`（非负整数）。全工程唯一的实现（F7，审查 R1-15）：Node 的 ID 里的毫秒
//! （node-store 的 `NodeIds`、part / 消息 ID）与 `sanitizePath` 的哈希后缀（paths）都用它。以前有 4 份拷贝。

/// 36 进制的数字（小写），`hash::short_id` 共用。
pub(crate) const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
/// 进制。
pub(crate) const RADIX: u64 = 36;

/// 非负整数的 36 进制小写写法，与 JavaScript `(value).toString(36)` 相同；0 是 `"0"`。
pub fn base36(mut value: u64) -> String {
    let mut digits = Vec::new();
    loop {
        digits.push(DIGITS[(value % RADIX) as usize]);
        value /= RADIX;
        if value == 0 {
            break;
        }
    }
    digits.reverse();
    digits.into_iter().map(char::from).collect()
}

#[cfg(test)]
#[path = "radix_tests.rs"]
mod tests;
