//! 屏幕上的矩形（docs/specs/rust-cli/tui-kit.md §3.2）：可点击区域、transcript 的可见区、输入框的文字区都按行列记。
//! 坐标是屏幕的行列（0 起），`width` / `height` 是格数。

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn top(self) -> u16 {
        self.y
    }

    /// 右边界（不含）。
    pub const fn right(self) -> u16 {
        self.x.saturating_add(self.width)
    }

    /// 下边界（不含）。
    pub const fn bottom(self) -> u16 {
        self.y.saturating_add(self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::Rect;

    #[test]
    fn edges() {
        let r = Rect::new(2, 3, 4, 2);
        assert_eq!((r.top(), r.right(), r.bottom()), (3, 6, 5));
    }
}
