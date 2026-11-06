//! 测试用的替身（feature `testing`）：终端解码器、假终端、宽度表的锁。
pub mod fake_terminal;
pub mod vt;
pub mod width_lock;

pub use vt::{Vt, VtCell, VtError};

#[cfg(test)]
mod vt_tests;
