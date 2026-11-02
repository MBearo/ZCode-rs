//! 同一种写入只有一个在途（架构整理 U13）：在途期间又来的只留最后一个，在途的完成后再写，两次写入不交错
//! （tui.md §7.1、tui-manage.md §2.1）。以前界面语言与主题各写一份 `writing: bool` 加 `next: Option<T>`。

pub(super) struct Latest<T> {
    writing: bool,
    next: Option<T>,
}

impl<T> Default for Latest<T> {
    fn default() -> Self {
        Self {
            writing: false,
            next: None,
        }
    }
}

impl<T> Latest<T> {
    /// 要写 `value`：没有在途的时返回它，调用方开始写；有在途的时记下（换掉更早记下的），返回 None。
    pub(super) fn start(&mut self, value: T) -> Option<T> {
        if self.writing {
            self.next = Some(value);
            return None;
        }
        self.writing = true;
        Some(value)
    }

    /// 在途的写完了：返回在途期间最后记下的那个（调用方经 [`Self::start`] 接着写它）。
    pub(super) fn finish(&mut self) -> Option<T> {
        self.writing = false;
        self.next.take()
    }
}

#[cfg(test)]
mod tests {
    use super::Latest;

    #[test]
    fn only_the_last_value_waits_for_the_write_in_flight() {
        let mut latest = Latest::default();
        assert_eq!(latest.start(1), Some(1));
        assert_eq!(latest.start(2), None);
        assert_eq!(latest.start(3), None);
        assert_eq!(latest.finish(), Some(3));
        assert_eq!(latest.start(3), Some(3));
        assert_eq!(latest.finish(), None);
        assert_eq!(latest.start(4), Some(4));
    }
}
