//! Windows 的前景 / 背景色探测（docs/specs/rust-cli/tui.md §7.4 的 T13，工作线 F71）：照 Unix 的 OSC 10 / 11 探测，
//! 回复从控制台的输入记录里读。
//!
//! 缺陷修复（T13）：以前 Windows 不探测，前景与背景都不知道，浅色主题的 Windows Terminal 也按深色配色。
//!
//! 拼回复、判断完整与停止的逻辑不依赖平台（[`collect`]、[`parse`]），测试在 macOS 上注入输入记录；Win32 的调用只在
//! Windows 上编译。

use std::time::{Duration, Instant};

use super::palette::{Probed, parse_osc_color};

/// 写给终端的查询：OSC 10（前景）与 OSC 11（背景）。不带 DA1：经 ConPTY 时 DA1 可能由 conhost 先回，早于转给终端的
/// OSC 回复，读到它就停会漏掉颜色。
pub(crate) const QUERY: &[u8] = b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\";

/// 回复最多收这么多字节（两段回复只有几十个字节；再多是探测期间的大段输入）。
const MAX_REPLY: usize = 1024;

/// 控制台输入记录里探测要看的部分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Record {
    /// 按下事件带的 UTF-16 码元（没有字符时是 0）。
    KeyDown(u16),
    /// 松开、鼠标、焦点、窗口大小等。
    Other,
}

/// 控制台的输入队列。
pub(crate) trait ConsoleInput {
    /// 等到队列里有记录，或者过了 `timeout`；有记录时为 true。出错当作没有。
    fn wait(&mut self, timeout: Duration) -> bool;
    /// 读出已经到了的记录（不等待）；出错时为空。
    fn read(&mut self) -> Vec<Record>;
}

/// 收集回复：按下事件的 ASCII 字符按顺序拼起来，直到前景与背景两段都完整，或者过了 `timeout`。其他记录与
/// 非 ASCII 字符丢掉（与 Unix 相同，探测期间到达的别的输入不保留）。
pub(crate) fn collect(input: &mut dyn ConsoleInput, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    let mut reply = String::new();
    while !(complete(&reply, "10").is_some() && complete(&reply, "11").is_some())
        && reply.len() < MAX_REPLY
    {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || !input.wait(left) {
            break;
        }
        for record in input.read() {
            if let Record::KeyDown(unit) = record
                && let Ok(byte) = u8::try_from(unit)
                && byte.is_ascii()
                && byte != 0
            {
                reply.push(char::from(byte));
            }
        }
    }
    reply
}

/// 回复里 `ESC ] <code> ;` 开头、以 BEL 或 ST（`ESC \`）结尾的完整一段。
fn complete<'a>(reply: &'a str, code: &str) -> Option<&'a str> {
    let start = reply.find(&format!("\x1b]{code};"))?;
    let rest = &reply[start..];
    let bell = rest.find('\x07').map(|at| at + 1);
    const ST: &str = "\x1b\\";
    let st = rest.find(ST).map(|at| at + ST.len());
    let end = match (bell, st) {
        (Some(bell), Some(st)) => bell.min(st),
        (end, None) | (None, end) => end?,
    };
    Some(&rest[..end])
}

/// 完整的那几段回复里的颜色；不完整的不算。
pub(crate) fn parse(reply: &str) -> Probed {
    Probed {
        foreground: complete(reply, "10").and_then(parse_osc_color),
        background: complete(reply, "11").and_then(parse_osc_color),
    }
}

/// 真实的控制台：打开 stdout 的 VT 处理、写查询、读 stdin 的输入记录。不是控制台或打不开 VT 处理（旧的控制台）时
/// 不探测：否则查询会被原样打到屏幕上。
#[cfg(windows)]
pub(crate) fn probe(timeout: Duration) -> Probed {
    use std::io::Write;

    let Some(mut console) = windows::Console::open() else {
        return Probed::default();
    };
    let mut out = std::io::stdout();
    if out.write_all(QUERY).and_then(|()| out.flush()).is_err() {
        return Probed::default();
    }
    parse(&collect(&mut console, timeout))
}

#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "控制台的模式与输入记录是 Win32 的 FFI，没有安全封装"
)]
mod windows {
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Console::{
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetNumberOfConsoleInputEvents,
        GetStdHandle, INPUT_RECORD, KEY_EVENT, ReadConsoleInputW, STD_INPUT_HANDLE,
        STD_OUTPUT_HANDLE, SetConsoleMode,
    };
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    use super::{ConsoleInput, Record};

    /// 一次最多读这么多条记录。
    const BATCH: usize = 64;

    /// stdin 的控制台输入（stdout 的 VT 处理已打开）。
    pub(super) struct Console {
        input: HANDLE,
    }

    fn handle(kind: u32) -> Option<HANDLE> {
        // SAFETY: 只取本进程的标准句柄。
        let handle = unsafe { GetStdHandle(kind) };
        (!handle.is_null() && handle != INVALID_HANDLE_VALUE).then_some(handle)
    }

    impl Console {
        pub(super) fn open() -> Option<Self> {
            let input = handle(STD_INPUT_HANDLE)?;
            let output = handle(STD_OUTPUT_HANDLE)?;
            let mut mode = 0;
            // SAFETY: 句柄来自 GetStdHandle；`mode` 是本函数里的局部变量。
            if unsafe { GetConsoleMode(input, &mut mode) } == 0 {
                return None;
            }
            // SAFETY: 同上。
            if unsafe { GetConsoleMode(output, &mut mode) } == 0 {
                return None;
            }
            if mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING == 0 {
                // 与 crossterm 进入界面时做的相同（之后画屏也要它）；打不开时是旧的控制台。
                // SAFETY: 只给 stdout 的控制台加一个模式位。
                if unsafe { SetConsoleMode(output, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) } == 0
                {
                    return None;
                }
            }
            Some(Self { input })
        }
    }

    impl ConsoleInput for Console {
        fn wait(&mut self, timeout: Duration) -> bool {
            let millis = u32::try_from(timeout.as_millis())
                .unwrap_or(u32::MAX)
                .max(1);
            // SAFETY: 等待 stdin 的控制台句柄，不改它。
            unsafe { WaitForSingleObject(self.input, millis) == WAIT_OBJECT_0 }
        }

        fn read(&mut self) -> Vec<Record> {
            let mut pending = 0;
            // SAFETY: `pending` 是局部变量；句柄是 stdin 的控制台。
            if unsafe { GetNumberOfConsoleInputEvents(self.input, &mut pending) } == 0
                || pending == 0
            {
                return Vec::new();
            }
            let mut records = [INPUT_RECORD::default(); BATCH];
            let wanted = u32::try_from(BATCH).unwrap_or(u32::MAX).min(pending);
            let mut read = 0;
            // SAFETY: 缓冲区有 BATCH 条，`wanted` 不超过它；队列里至少有 `wanted` 条，不会阻塞。
            if unsafe { ReadConsoleInputW(self.input, records.as_mut_ptr(), wanted, &mut read) }
                == 0
            {
                return Vec::new();
            }
            records
                .iter()
                .take(usize::try_from(read).unwrap_or(0))
                .map(|record| {
                    if u32::from(record.EventType) != KEY_EVENT {
                        return Record::Other;
                    }
                    // SAFETY: EventType 是 KEY_EVENT 时联合体里是按键记录；ReadConsoleInputW 写的是 UnicodeChar。
                    let key = unsafe { record.Event.KeyEvent };
                    if key.bKeyDown == 0 {
                        Record::Other
                    } else {
                        // SAFETY: 同上。
                        Record::KeyDown(unsafe { key.uChar.UnicodeChar })
                    }
                })
                .collect()
        }
    }
}

#[cfg(test)]
#[path = "probe_console_tests.rs"]
mod tests;
