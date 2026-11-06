//! 一个终端格子打包进一个 `u64`：格子网是一片 `Vec<Cell>`，比较是一条指令。
//! 移植自 packages/tui/tui/src/zcode/tui/render/Cell.scala（feat/tui a2bf878b34）。
//!
//! ```text
//!   bits  0..20  码点（21 位，到 U+10FFFF）
//!   bits 21..52  style id（32 位，StylePool 的下标）
//!   bits 53..61  （没用）
//!   bits 62..63  宽度类
//! ```

/// 一个格子占几列、是什么角色。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum WidthClass {
    /// 占一列。
    Narrow = 0,
    /// 两列字形（宽字、全角、emoji）的首格。
    Wide = 1,
    /// [`WidthClass::Wide`] 的尾格；自己不写出任何东西。
    Spacer = 2,
    /// 跟在基字后面的组合 / 零宽记号。
    ZeroWidth = 3,
}

impl WidthClass {
    fn from_bits(bits: u64) -> WidthClass {
        match bits & 0x3 {
            0 => WidthClass::Narrow,
            1 => WidthClass::Wide,
            2 => WidthClass::Spacer,
            _ => WidthClass::ZeroWidth,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Cell(u64);

const CP_MASK: u64 = 0x1f_ffff;
const STYLE_MASK: u64 = 0xffff_ffff;

impl Cell {
    /// 空格子：默认样式的一个空格。也是"擦掉"的值。
    pub const BLANK: Cell = Cell::pack(' ', 0, WidthClass::Narrow);
    /// 任何真实格子都打包不出的值：与一切都不等（渲染器把图片底下的格子标成它）。
    pub const UNKNOWN: Cell = Cell(u64::MAX);

    pub const fn pack(cp: char, style_id: u32, class: WidthClass) -> Cell {
        Cell(
            (cp as u64 & CP_MASK)
                | ((style_id as u64 & STYLE_MASK) << 21)
                | ((class as u64 & 0x3) << 62),
        )
    }

    pub const fn raw(self) -> u64 {
        self.0
    }

    /// 格子的码点；[`Cell::UNKNOWN`] 没有码点，给出替换字符。
    pub fn code_point(self) -> char {
        char::from_u32((self.0 & CP_MASK) as u32).unwrap_or(char::REPLACEMENT_CHARACTER)
    }

    pub const fn style_id(self) -> u32 {
        ((self.0 >> 21) & STYLE_MASK) as u32
    }

    pub fn width_class(self) -> WidthClass {
        WidthClass::from_bits(self.0 >> 62)
    }

    pub const fn is_spacer(self) -> bool {
        (self.0 >> 62) & 0x3 == WidthClass::Spacer as u64
    }
}
