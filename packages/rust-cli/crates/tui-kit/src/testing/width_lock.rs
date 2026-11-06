//! 类宽度采纳（`width::adopt`）是进程全局的，测试并行跑：会采纳的测试拿 [`WIDTHS`] 的写锁、在 [`Adopted`] 的 drop 里还原
//! 缺省；会铺含 emoji / 模糊字形的行的测试拿读锁（[`widths`]），就不会在别人采纳期间铺错。整个 crate 共用这一把锁。
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use rust_cli_base::sync::RwLockUnpoisoned;

use crate::render::width;

pub static WIDTHS: RwLock<()> = RwLock::new(());

/// 类宽度保持缺省的期间（读锁）。
pub fn widths() -> RwLockReadGuard<'static, ()> {
    WIDTHS.read_unpoisoned()
}

/// 采纳了类宽度的期间：独占 [`WIDTHS`]，drop 时先还原缺省再放锁。
pub struct Adopted {
    _lock: RwLockWriteGuard<'static, ()>,
}

impl Adopted {
    pub fn new(emoji_bmp: u32, emoji_astral: u32, ambiguous: u32) -> Adopted {
        let lock = WIDTHS.write_unpoisoned();
        width::adopt(emoji_bmp, emoji_astral, ambiguous);
        Adopted { _lock: lock }
    }

    /// 只独占，不采纳：调用方自己（经握手、运行时）采纳。
    pub fn exclusive() -> Adopted {
        Adopted {
            _lock: WIDTHS.write_unpoisoned(),
        }
    }
}

impl Drop for Adopted {
    fn drop(&mut self) {
        width::reset_adopted();
    }
}
