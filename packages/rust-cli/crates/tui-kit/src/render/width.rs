//! Unicode 文字在终端里的显示宽度（列数）。布局与渲染器共用的唯一来源：两边对一个字形的宽度不一致，格子就错位。
//! 移植自 packages/tui/tui/src/zcode/tui/render/Width.scala（feat/tui a2bf878b34）；表在 `width_tables.rs`，由脚本从
//! Scala 生成。
//!
//! 每个码点是 0（组合 / 零宽）、1 或 2（东亚宽 / 全角 / emoji）列。表按 Unicode 16.0 生成，按真实终端对它们的一致程度
//! 分类：
//!
//! - 零宽：Mn / Me / Cf（U+00AD 软连字符除外）、韩文的中声 / 终声字母，以及（有意的选择）emoji 肤色修饰符 U+1F3FB–1F3FF。
//! - 宽：emoji 区块以外的 East Asian Width `W` / `F`——CJK、韩文、假名、全角。各代终端都认。
//! - emoji（BMP / 辅助平面）：emoji 区块里的 EAW `W`（✅ ⚡ ⏰ / 😀 🚀 🟢）。新终端画宽，旧的 wcwidth（macOS libc、
//!   Terminal.app）画窄——所以按类可以 [`adopt`]。
//! - 模糊：EAW `A`（制表符、希腊 / 西里尔字母、①、PUA / Nerd Font…）。缺省窄，CJK 配置的终端画宽——可以 [`adopt`]。
//! - 其余：1。
//!
//! 字素簇按构造处理而不建模：组簇的码点（ZWJ、变体选择符、肤色修饰符）都是零宽，`Surface::build` 写出时丢掉零宽码点——
//! 终端只收到算过宽度的字符，会组簇的终端不能背着模型合并格子。代价是组合 emoji 的画面降级（ZWJ 家庭画成几个部分，
//! 肤色与 VS16 被剥掉）。已知的残留：地区指示符成对（旗帜）算 1+1=2，与把两个合成一个双宽字形的终端（Ghostty）相符，
//! 与各画一个双宽的不符。
use std::sync::atomic::{AtomicU32, Ordering};

use super::width_tables as tables;

/// 三个有争议的宽度类各算几列（docs/specs/rust-cli/tui-kit.md §3.6）：纯值，`display_width` 按它算。缺省与现代终端
/// 相同（Ghostty、kitty、WezTerm、iTerm2、foot）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Widths {
    pub emoji_bmp: usize,
    pub emoji_astral: usize,
    pub ambiguous: usize,
}

impl Widths {
    pub const DEFAULT: Widths = Widths {
        emoji_bmp: 2,
        emoji_astral: 2,
        ambiguous: 1,
    };

    /// 采纳终端量到的意见（启动时的光标位置探测）：只有 1 和 2 是合理的值，别的按类忽略、保持原值。
    pub fn adopt(self, emoji_bmp: u32, emoji_astral: u32, ambiguous: u32) -> Widths {
        let sane = |v: u32, keep: usize| if v == 1 || v == 2 { v as usize } else { keep };
        Widths {
            emoji_bmp: sane(emoji_bmp, self.emoji_bmp),
            emoji_astral: sane(emoji_astral, self.emoji_astral),
            ambiguous: sane(ambiguous, self.ambiguous),
        }
    }

    pub fn display_width(self, cp: char) -> usize {
        let c = cp as u32;
        // C0 / C1 控制字符在格子网里不占列。
        if c < 0x20 || (0x7f..0xa0).contains(&c) {
            0
        } else if c < 0xa1 {
            1 // ASCII 快路径：在第一个模糊字符之前
        } else if in_table(c, tables::ZERO_WIDTH) {
            0
        } else if in_table(c, tables::WIDE) {
            2
        } else if in_table(c, tables::EMOJI_BMP) {
            self.emoji_bmp
        } else if in_table(c, tables::EMOJI_ASTRAL) {
            self.emoji_astral
        } else if in_table(c, tables::AMBIGUOUS) {
            self.ambiguous
        } else {
            1
        }
    }

    pub fn string_width(self, s: &str) -> usize {
        s.chars().map(|c| self.display_width(c)).sum()
    }
}

/// 在有序、互不相交的闭区间表里二分查找。
fn in_table(cp: u32, table: &[(u32, u32)]) -> bool {
    table
        .binary_search_by(|&(lo, hi)| {
            if cp < lo {
                std::cmp::Ordering::Greater
            } else if cp > hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// 终端之间已知对这个码点的宽度有分歧（emoji / 象形区块：旧 wcwidth 说 1 的新终端说 2，文字呈现的象形相反）。
/// 渲染器给含这种字形的行整行重写——从第 0 列写起、擦掉尾巴——有分歧的终端最多错画这一行自己的尾巴，不会弄脏整屏。
/// ASCII 快路径让普通的行不花这个检查。
pub fn risky(cp: char) -> bool {
    let c = cp as u32;
    c >= 0x2139 && in_table(c, tables::RISKY)
}

// 进程里采纳的类宽度（三个 u8 打包）与它的代数：单个渲染线程读写，但 tokio 可能把循环任务换到别的线程，所以是原子，
// 不是 thread_local。纯值的算法在 `Widths`，测试只用它，不碰这里。
static ADOPTED: AtomicU32 = AtomicU32::new(pack(Widths::DEFAULT));
static GENERATION: AtomicU32 = AtomicU32::new(0);

const fn pack(w: Widths) -> u32 {
    (w.emoji_bmp as u32) | ((w.emoji_astral as u32) << 8) | ((w.ambiguous as u32) << 16)
}

fn unpack(bits: u32) -> Widths {
    Widths {
        emoji_bmp: (bits & 0xff) as usize,
        emoji_astral: ((bits >> 8) & 0xff) as usize,
        ambiguous: ((bits >> 16) & 0xff) as usize,
    }
}

/// 现在采纳的类宽度。
pub fn current() -> Widths {
    unpack(ADOPTED.load(Ordering::Relaxed))
}

/// 标识现在的类宽度：代数相同，文字铺出来相同。布局的记忆按它做键，缺省宽度下填的记忆在之后的 [`adopt`] 后重算。
pub fn generation() -> u32 {
    GENERATION.load(Ordering::Relaxed)
}

/// 采纳终端量到的意见。宽度烤进铺好的行与画过的帧，首帧之后改了必须跟着整屏重画（运行时在启动握手落定时做，
/// docs/puffin-tui.md §6 规则 3）；布局的记忆经 [`generation`] 看到变化。不合理的值按类忽略。
pub fn adopt(emoji_bmp: u32, emoji_astral: u32, ambiguous: u32) {
    let before = current();
    let after = before.adopt(emoji_bmp, emoji_astral, ambiguous);
    if after != before {
        ADOPTED.store(pack(after), Ordering::Relaxed);
        GENERATION.fetch_add(1, Ordering::Relaxed);
    }
}

/// 回到缺省的类宽度（只给测试）。
pub fn reset_adopted() {
    ADOPTED.store(pack(Widths::DEFAULT), Ordering::Relaxed);
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// `cp` 按现在采纳的类宽度占几列。
pub fn display_width(cp: char) -> usize {
    current().display_width(cp)
}

pub fn string_width(s: &str) -> usize {
    current().string_width(s)
}
