//! 先建好、后接上一次的值：组装时打破构造环（docs/specs/rust-cli/architecture.md §2）。
//!
//! 组装根里有几处真递归（A 要用 B、B 又要用 A），只能先把一方交出去、另一方建好后再接上。这里是它们共用的一个显式
//! 机制（工作线 F131：以前三处各自用 `OnceLock`，有的接两次时静默忽略）：只接一次；读的一方在接上之前读到 None，
//! 由它决定报错还是跳过；接第二次是组装的缺陷，[`Late::bind`] 交回新值，调用方记 error 日志。只在组装时接，之后只读。

use std::sync::OnceLock;

/// 先建好、后接上一次的值。
#[derive(Debug)]
pub struct Late<T>(OnceLock<T>);

impl<T> Late<T> {
    pub const fn new() -> Self {
        Self(OnceLock::new())
    }

    /// 接上。已经接过时留着第一个，把这一个交回（组装接了两次）。
    pub fn bind(&self, value: T) -> Result<(), T> {
        self.0.set(value)
    }

    /// 接上的值；组装还没接上时为 None。
    pub fn get(&self) -> Option<&T> {
        self.0.get()
    }
}

impl<T> Default for Late<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "late_tests.rs"]
mod tests;
