//! 随机数的入口（修复线 F58，README §4 的“随机”类别）：`RandomState` 的随机种子，不要求密码学强度。fs 的临时文件名、
//! 锁的随机串与 token，net 的退避抖动都从这里取。要加密强度的随机（凭据的 IV、OAuth 轮询的令牌）在 platform（ring）。
//! `clippy.toml` 禁 `std::hash::RandomState::new` 与 `ring::rand::SystemRandom::new`，入口是这里与 platform。

use std::hash::{BuildHasher, DefaultHasher, Hasher, RandomState};

/// 一个随机种子的哈希器：调用方再写入计数器、时间等，区分同一进程内与 pid 复用后的进程。
#[expect(
    clippy::disallowed_methods,
    reason = "base 的 random 是随机数的入口（README §4，F58）"
)]
pub fn hasher() -> DefaultHasher {
    RandomState::new().build_hasher()
}

/// 一个随机的 u64：每个新的 `RandomState` 的密钥都不同，空输入的哈希值因此每次不同。
pub fn u64() -> u64 {
    hasher().finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两次取值不同（密钥每次不同；相同的概率可以忽略）。
    #[test]
    fn values_differ() {
        assert_ne!(u64(), u64());
        let mut first = hasher();
        let mut second = hasher();
        first.write_u64(1);
        second.write_u64(1);
        assert_ne!(first.finish(), second.finish());
    }
}
