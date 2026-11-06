//! 原始字节流到 [`Key`] 的增量解码器（docs/puffin-tui.md §4.3「按键解码」）。
//! 移植自 packages/tui/tui/src/zcode/tui/app/KeyParser.scala（feat/tui a2bf878b34）。
//!
//! 字节可能被拆在几次读里到达：解码器缓存不完整的 UTF-8 码点与不完整的转义序列，完整了才产出键。状态机、各分支的
//! 顺序与产出都与 Scala 相同；文字按 UTF-8 解成 `char`，不经 UTF-16。
//!
//! 运行时怎么驱动它：
//!
//! - 每次读到字节就 [`KeyParser::feed`]（或 [`KeyParser::feed_into`]），按顺序处理产出的键。
//! - 一批字节喂完后若 [`KeyParser::pending_escape`]，记下这时的输入序号，在 [`ESCAPE_TIMEOUT`] 之后检查：其间没有
//!   新字节到达且仍在等，就调用 [`KeyParser::flush_escape`]，把产出的键当作普通按键处理。单独的 Esc 与 Alt+键、
//!   方向键序列的开头在下一个字节到来之前分不出来；这是传统终端编码自身的歧义，登记过的超时例外
//!   （docs/puffin-tui.md §4.1 的运行时）。
use std::time::Duration;

use super::handshake::parse_color;
use super::key::Key;

/// ESC 之后多久没有下一个字节就算单独的 Esc（docs/puffin-tui.md §4.3）。只有传统编码需要它。
pub const ESCAPE_TIMEOUT: Duration = Duration::from_millis(50);

/// 等的最长的 OSC 10/11 回答：`ESC ] 11;rgba:ffff/ffff/ffff/ffff ESC \` 是 37 字节。
const MAX_REPLY: usize = 64;

/// 括号粘贴的结束标记 `ESC [ 2 0 1 ~` 的原始字节。
const PASTE_END: &[u8; 6] = b"\x1b[201~";

/// 增量解码器。一个终端一个，跨读保留状态。
#[derive(Debug, Default)]
pub struct KeyParser {
    /// 进行中的转义序列或多字节 UTF-8 码点的字节。
    buf: Vec<u8>,
    /// kitty 协议报告的 Shift 是否按着（单独按下的 Shift 是事件，不是键）：Shift+⏎ 换行靠它。
    shift_held: bool,
    /// 裸的 X10 鼠标开头（`ESC [ M`）之后还要丢掉的原始字节数。我们从不打开 X10，但配置错的终端会送出这个开头，后跟
    /// 三个坐标字节；它们可能小于 0x20，不丢掉就会作为乱码漏进输入框。这个计数正好丢掉那三个。
    swallow: u8,
    /// 括号粘贴（`CSI 200~` … `CSI 201~`）进行中：字节原样收进 `paste_buf` 而不解码成键，结束标记完整时整段作为一个
    /// [`Key::Paste`] 产出。
    pasting: bool,
    paste_buf: Vec<u8>,
    /// 最近的字节已经匹配了结束标记的多少字节。部分匹配先扣着，断掉时作为普通内容放回 `paste_buf`。
    paste_tail: usize,
}

impl KeyParser {
    pub fn new() -> KeyParser {
        KeyParser::default()
    }

    /// 有一个 ESC、`ESC [`、`ESC O` 或 `ESC ]` 在等下一个字节。单独的 Esc 与 Alt+键或序列的开头在下一个字节之前看起来
    /// 一样；没有字节按时到达时，运行时调用 [`KeyParser::flush_escape`]。
    pub fn pending_escape(&self) -> bool {
        self.buf.first() == Some(&0x1b) && self.buf.len() <= 2
    }

    /// 等着的 ESC 后面没有字节：它是 Esc（或 Alt+`[` / Alt+`O` / Alt+`]`）。没有在等时返回空。
    pub fn flush_escape(&mut self) -> Vec<Key> {
        if !self.pending_escape() {
            return Vec::new();
        }
        let key = match self.buf.get(1) {
            None => Key::Esc,
            Some(&b) => Key::alt(Key::Char(char::from(b))),
        };
        self.buf.clear();
        vec![key]
    }

    /// 喂一段字节，返回它们完成的键（按到达顺序）。
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Key> {
        let mut out = Vec::new();
        self.feed_into(bytes, &mut out);
        out
    }

    /// 同 [`KeyParser::feed`]，键追加到 `out`。
    pub fn feed_into(&mut self, bytes: &[u8], out: &mut Vec<Key>) {
        for &b in bytes {
            self.byte(b, out);
        }
    }

    fn byte(&mut self, b: u8, out: &mut Vec<Key>) {
        if self.swallow > 0 {
            self.swallow -= 1;
        } else if self.pasting {
            self.paste(b, out);
        } else if self.buf.is_empty() {
            self.start(b, out);
        } else if self.buf[0] == 0x1b {
            self.escape(b, out);
        } else {
            self.utf8(b, out);
        }
    }

    fn start(&mut self, b: u8, out: &mut Vec<Key>) {
        match b {
            // ESC：序列开始；UTF-8 首字节：码点开始。
            0x1b | 0xc0..=0xff => self.buf.push(b),
            0x0d => out.push(if self.shift_held {
                Key::Newline
            } else {
                Key::Enter
            }),
            0x0a => out.push(Key::Newline),
            0x7f | 0x08 => out.push(Key::Backspace),
            0x09 => out.push(Key::Tab),
            0x01..=0x1a => out.push(Key::Ctrl(char::from(b'A' + b - 1))),
            0x20..=0x7e => out.push(Key::Char(char::from(b))),
            // 游离的续字节或 NUL：忽略。
            _ => {}
        }
    }

    fn escape(&mut self, b: u8, out: &mut Vec<Key>) {
        if self.buf.len() >= 2 && self.buf[1] == b']' {
            self.osc(b, out);
        } else if self.buf.len() == 1 {
            // 目前只有 ESC。
            if b == b'[' || b == b'O' || b == b']' {
                // CSI / SS3 / OSC 的引导。
                self.buf.push(b);
                return;
            }
            // 传统的 Alt：终端在键自己的字节前送一个 ESC。
            self.buf.clear();
            match b {
                // Esc，这个 ESC 接着等。
                0x1b => {
                    self.buf.push(b);
                    out.push(Key::Esc);
                }
                0x7f | 0x08 => out.push(Key::alt(Key::Backspace)),
                0x0d => out.push(Key::alt(Key::Enter)),
                0x20..=0x7e => out.push(Key::alt(Key::Char(char::from(b)))),
                // 单独的 ESC，再重新处理 b。
                _ => {
                    out.push(Key::Esc);
                    self.start(b, out);
                }
            }
        } else {
            self.buf.push(b);
            if (0x40..=0x7e).contains(&b) {
                let seq = std::mem::take(&mut self.buf);
                self.parse_sequence(&seq, out);
            }
            // 否则还在收参数。
        }
    }

    /// `ESC ] …` 的又一个字节：终端对 OSC 10/11 的回答（`ESC ] 1 0|1 ; rgb:… BEL|ST`），外观变化时问的
    /// （docs/puffin-tui.md §4.2「主题」）。回答一次成串到达，不像任何打出的东西，所以第一个对不上的字节说明它是
    /// Alt+]，它之后的字节重新当输入。
    fn osc(&mut self, b: u8, out: &mut Vec<Key>) {
        let at = self.buf.len();
        let last = self.buf[at - 1];
        if b == 0x07 && at > 5 {
            self.finish_osc(out);
        } else if last == 0x1b {
            if b == b'\\' {
                self.finish_osc(out);
            } else {
                self.not_osc(b, out);
            }
        } else if b == 0x1b && at > 5 {
            self.buf.push(b);
        } else {
            let fits = match at {
                2 => b == b'1',
                3 => b == b'0' || b == b'1',
                4 => b == b';',
                _ => (0x20..0x7f).contains(&b) && at < MAX_REPLY,
            };
            if fits {
                self.buf.push(b);
            } else {
                self.not_osc(b, out);
            }
        }
    }

    fn finish_osc(&mut self, out: &mut Vec<Key>) {
        // buf 里 OSC 的部分只收过 0x20..0x7e 与结尾的 ESC，所以按 Latin-1 转成字符串不会出错。
        let body: String = self.buf[2..]
            .iter()
            .take_while(|&&c| c != 0x07 && c != 0x1b)
            .map(|&c| char::from(c))
            .collect();
        self.buf.clear();
        let slot = body
            .get(..2)
            .and_then(|s| s.parse::<u8>().ok())
            .unwrap_or(0);
        out.push(match parse_color(body.get(3..).unwrap_or("")) {
            Some(rgb) => Key::ColorReport { slot, rgb },
            None => Key::Unknown,
        });
    }

    fn not_osc(&mut self, b: u8, out: &mut Vec<Key>) {
        let mut again = self.buf.split_off(2);
        again.push(b);
        self.buf.clear();
        out.push(Key::alt(Key::Char(']')));
        for c in again {
            self.byte(c, out);
        }
    }

    /// 解一个完整的 `ESC [ … final` 或 `ESC O final`。
    fn parse_sequence(&mut self, seq: &[u8], out: &mut Vec<Key>) {
        let fin = seq[seq.len() - 1];
        let params: String = seq[2..seq.len() - 1]
            .iter()
            .map(|&c| char::from(c))
            .collect();
        match fin {
            b'A' | b'B' | b'C' | b'D' | b'H' | b'F' => parse_functional(fin, &params, out),
            b'~' => self.parse_tilde(&params, out),
            b'u' => self.parse_csi_u(&params, out),
            // SGR 1006 鼠标报告 `ESC [ < b ; x ; y M|m`；`params` 仍带着开头的 `<`。`M` 是按下，`m` 是松开。
            b'M' | b'm' if seq[2] == b'<' => parse_sgr_mouse(&params, fin == b'M', out),
            b'Z' if params.is_empty() => out.push(Key::BackTab),
            b'I' if params.is_empty() => out.push(Key::FocusIn),
            b'n' if params == "?997;1" => out.push(Key::Appearance { dark: true }),
            b'n' if params == "?997;2" => out.push(Key::Appearance { dark: false }),
            b'O' if params.is_empty() && seq[1] == b'[' => out.push(Key::FocusOut),
            // 裸的 `ESC [ M`：X10 的开头，丢掉它的 3 个坐标字节。
            b'M' if seq.len() == 3 && seq[1] == b'[' => self.swallow = 3,
            _ => out.push(Key::Unknown),
        }
    }

    fn parse_tilde(&mut self, params: &str, out: &mut Vec<Key>) {
        if is_release(params) {
            return;
        }
        // 按数字头匹配，带修饰的形式（`5;2` 等）解成同一个键；尾部（修饰 / 事件）只给 is_release 与 with_modifier 用。
        let key = match params.split(';').next().unwrap_or("") {
            "1" | "7" => Key::Home,
            "4" | "8" => Key::End,
            "3" => Key::Delete,
            "5" => Key::PageUp,
            "6" => Key::PageDown,
            // 括号粘贴开始。
            "200" => {
                self.pasting = true;
                return;
            }
            // 没有粘贴在进行时游离的结束标记。
            "201" => return,
            _ => return self.parse_modify_other_keys(params, out),
        };
        out.push(with_modifier(key, params));
    }

    /// 收一个粘贴的字节，留意结束标记。
    fn paste(&mut self, b: u8, out: &mut Vec<Key>) {
        if b == PASTE_END[self.paste_tail] {
            self.paste_tail += 1;
            if self.paste_tail == PASTE_END.len() {
                self.finish_paste(out);
            }
        } else if self.paste_tail > 0 {
            // 结束标记的部分匹配断了：它终究是粘贴的内容。b 再处理一次——它自己可能开始新的匹配。
            self.paste_buf
                .extend_from_slice(&PASTE_END[..self.paste_tail]);
            self.paste_tail = 0;
            self.paste(b, out);
        } else {
            self.paste_buf.push(b);
        }
    }

    /// 把收好的粘贴解码成一个 [`Key::Paste`]。换行统一成 LF；其他控制字节（包括终端放过来的 ESC）丢掉，粘贴永远不会
    /// 重放成按键，也不会把活的转义序列漏进输入。什么都没粘是 `Paste("")`：剪贴板里只有图片时，有的终端对 ⌘V 送这个
    /// （docs/puffin-tui.md §4.2「图片」）；字节全被丢掉的粘贴什么也不产出。
    fn finish_paste(&mut self, out: &mut Vec<Key>) {
        let bytes = std::mem::take(&mut self.paste_buf);
        self.paste_tail = 0;
        self.pasting = false;
        let text: String = String::from_utf8_lossy(&bytes)
            .replace("\r\n", "\n")
            .chars()
            .map(|c| if c == '\r' { '\n' } else { c })
            .filter(|&c| {
                let cp = u32::from(c);
                cp == 9 || cp == 10 || (cp >= 32 && cp != 127)
            })
            .collect();
        if text.is_empty() && !bytes.is_empty() {
            return;
        }
        out.push(Key::Paste(text));
    }

    /// xterm 的 modifyOtherKeys 形式，tmux 没配成 CSI-u 时常发：`CSI 27 ; modifier ; codepoint ~`。
    fn parse_modify_other_keys(&self, params: &str, out: &mut Vec<Key>) {
        let parts: Vec<&str> = params.split(';').collect();
        if parts.len() < 3 || parts[0] != "27" {
            out.push(Key::Unknown);
            return;
        }
        let modifier = numeric_prefix(parts[1]).unwrap_or(1);
        out.push(self.modified_code(numeric_prefix(parts[2]), modifier));
    }

    /// 解应用绑定的那部分 CSI-u / kitty 按键事件。
    ///
    /// Shift+Enter 通常报成 `CSI 13;2u`；打开所有键报告时，不带修饰的 Enter 可能报成 `CSI 13u` 或 `CSI 13;1u`。
    fn parse_csi_u(&mut self, params: &str, out: &mut Vec<Key>) {
        let parts: Vec<&str> = params.split(';').collect();
        let code = numeric_prefix(parts[0]);
        let (modifier, event) = modifier_and_event(parts.get(1).copied().unwrap_or(""));

        if code.is_some_and(is_shift_key) {
            self.shift_held = event != 3 && has_shift(modifier);
        } else if code.is_some_and(is_modifier_key) || event == 3 {
            // 单独按下的修饰键、锁定键不是按键（docs/puffin-tui.md §4.3「按键解码」）；松开事件也不是。修复原因：把
            // Ctrl 单按映射成 Esc 的键盘每次先送一个单独的 Ctrl，它曾被解成 Key::Unknown，应用把它当成「下一个键」，
            // 清掉了 `esc rewind`，第二次 esc 永远打不开回退。
        } else {
            let text = associated_text(&parts);
            if text.is_empty() {
                out.push(self.modified_code(code, modifier));
            } else {
                out.extend(text);
            }
        }
    }

    fn modified_code(&self, code: Option<u32>, modifier: u32) -> Key {
        let Some(c) = code else {
            return Key::Unknown;
        };
        let shifted = has_shift(modifier) || self.shift_held;
        let ctrl = has_ctrl(modifier);
        let alt = has_alt(modifier);
        match c {
            13 if alt => Key::alt(Key::Enter),
            13 if shifted => Key::Newline,
            13 => Key::Enter,
            9 if shifted => Key::BackTab,
            9 => Key::Tab,
            8 | 127 => with_mods(Key::Backspace, modifier),
            27 => Key::Esc,
            0x61..=0x7a if ctrl => Key::Ctrl(char::from(c as u8).to_ascii_uppercase()),
            _ if is_functional_key(c) => Key::Unknown,
            // ⌘+字母属于终端（复制、粘贴……），从不编辑输入。
            _ if has_super(modifier) => Key::Unknown,
            0x20..=0x7e if alt && !ctrl => {
                let ch = char::from(c as u8);
                Key::alt(Key::Char(if shifted {
                    ch.to_ascii_uppercase()
                } else {
                    ch
                }))
            }
            _ if c >= 0x20 && !ctrl => typed(c),
            _ => Key::Unknown,
        }
    }

    fn utf8(&mut self, b: u8, out: &mut Vec<Key>) {
        if !(0x80..=0xbf).contains(&b) {
            // 不合法：放弃这个码点，把这个字节当新的开头再处理。
            self.buf.clear();
            self.start(b, out);
            return;
        }
        self.buf.push(b);
        let lead = self.buf[0];
        let expected = if lead >= 0xf0 {
            4
        } else if lead >= 0xe0 {
            3
        } else {
            2
        };
        if self.buf.len() >= expected {
            let cp = decode(&self.buf);
            self.buf.clear();
            // 修复原因：Scala 以前把高于 U+FFFF 的码点（emoji）在这里直接丢掉，输入框里打不出 emoji。
            if (0x20..=0x10ffff).contains(&cp)
                && let Some(ch) = char::from_u32(cp)
            {
                out.push(Key::Char(ch));
            }
        }
    }
}

fn parse_functional(fin: u8, params: &str, out: &mut Vec<Key>) {
    if is_release(params) {
        return;
    }
    let key = match fin {
        b'A' => Key::Up,
        b'B' => Key::Down,
        b'C' => Key::Right,
        b'D' => Key::Left,
        b'H' => Key::Home,
        b'F' => Key::End,
        _ => Key::Unknown,
    };
    out.push(with_modifier(key, params));
}

/// 给 `key` 套上 `CSI 1;mod X` / `CSI n;mod ~` 带的修饰（只有 shift 不改变什么；super 胜过 Ctrl，Ctrl 胜过 Alt）。
fn with_modifier(key: Key, params: &str) -> Key {
    let (modifier, _) = modifier_and_event(params.split(';').nth(1).unwrap_or(""));
    with_mods(key, modifier)
}

fn with_mods(key: Key, modifier: u32) -> Key {
    if key == Key::Unknown {
        key
    } else if has_super(modifier) {
        Key::with_super(key)
    } else if has_ctrl(modifier) {
        Key::with_ctrl(key)
    } else if has_alt(modifier) {
        Key::alt(key)
    } else {
        key
    }
}

/// 解 SGR-1006 鼠标报告的主体 `<b;x;y`（`press` 是结尾的 `M` 还是 `m`）。滚轮、按键的按下与松开、按着键的移动（拖动）
/// 报上去；不按键的悬停移动与水平滚轮丢掉。`x` / `y` 从 1 数起。主体不合法时是一个 [`Key::Unknown`]。
fn parse_sgr_mouse(body: &str, press: bool, out: &mut Vec<Key>) {
    let fields: Vec<Option<u32>> = body
        .strip_prefix('<')
        .unwrap_or(body)
        .split(';')
        .map(numeric_prefix)
        .collect();
    let &[Some(b), Some(x), Some(y)] = fields.as_slice() else {
        out.push(Key::Unknown);
        return;
    };
    let (col, row) = (x as usize, y as usize);
    let button = (b & 3) as u8;
    if b & 32 != 0 {
        // 移动报告。低位带着按着的键（0..2）→ 拖动；没有键的哨兵（3）是悬停（只有 `?1003`），丢掉。结尾是 `m` 的
        // 移动松开也丢掉。
        if press && button != 3 {
            out.push(Key::MouseDrag { button, col, row });
        }
    } else if b & 64 != 0 {
        // 滚轮：只算按下；水平滚轮（2/3）忽略。
        if press {
            match button {
                0 => out.push(Key::WheelUp { col, row }),
                1 => out.push(Key::WheelDown { col, row }),
                _ => {}
            }
        }
    } else if button != 3 {
        // 3 是没有键的松开哨兵。
        let mods = (b & 28) as u8;
        out.push(if press {
            Key::MousePress {
                button,
                col,
                row,
                mods,
            }
        } else {
            Key::MouseRelease {
                button,
                col,
                row,
                mods,
            }
        });
    }
}

fn is_release(params: &str) -> bool {
    let (_, event) = modifier_and_event(params.split(';').nth(1).unwrap_or(""));
    event == 3
}

fn modifier_bits(modifier: u32) -> u32 {
    modifier.saturating_sub(1)
}

fn has_shift(modifier: u32) -> bool {
    modifier_bits(modifier) & 1 != 0
}

fn has_alt(modifier: u32) -> bool {
    modifier_bits(modifier) & 2 != 0
}

fn has_ctrl(modifier: u32) -> bool {
    modifier_bits(modifier) & 4 != 0
}

fn has_super(modifier: u32) -> bool {
    modifier_bits(modifier) & 8 != 0
}

/// 打出的一个码点。代理区的值不是 `char`，解成 [`Key::Unknown`]（Scala 会产出一个孤立的 UTF-16 代理）。
fn typed(cp: u32) -> Key {
    char::from_u32(cp).map_or(Key::Unknown, Key::Char)
}

/// kitty 的关联文字（第三段，`:` 分隔的码点）：每个可打印码点一个 [`Key::Char`]。
fn associated_text(parts: &[&str]) -> Vec<Key> {
    let Some(text) = parts.get(2) else {
        return Vec::new();
    };
    text.split(':')
        .filter_map(numeric_prefix)
        .filter(|&cp| (0x20..=0x10ffff).contains(&cp) && !is_functional_key(cp))
        .filter_map(char::from_u32)
        .map(Key::Char)
        .collect()
}

/// kitty 把功能键编进私用区。
fn is_functional_key(code: u32) -> bool {
    (0xe000..=0xf8ff).contains(&code)
}

fn is_shift_key(code: u32) -> bool {
    matches!(code, 57441 | 57447 | 57453 | 57454)
}

/// kitty 给修饰键（Shift … Meta，左右两个，ISO level shift）与锁定键（Caps、Scroll、Num Lock）的码。
fn is_modifier_key(code: u32) -> bool {
    (57441..=57454).contains(&code) || (57358..=57360).contains(&code)
}

/// `modifier[:event]`，缺省都是 1。
fn modifier_and_event(s: &str) -> (u32, u32) {
    let mut parts = s.split(':');
    let modifier = parts.next().and_then(numeric_prefix).unwrap_or(1);
    let event = parts.next().and_then(numeric_prefix).unwrap_or(1);
    (modifier, event)
}

/// 开头的十进制数字。没有数字或超出 Scala `Int` 时是 None（与 `toIntOption` 相同）。
fn numeric_prefix(s: &str) -> Option<u32> {
    let end = s.bytes().take_while(u8::is_ascii_digit).count();
    s[..end]
        .parse::<i32>()
        .ok()
        .and_then(|v| u32::try_from(v).ok())
}

fn decode(bytes: &[u8]) -> u32 {
    let at = |i: usize| u32::from(bytes[i]);
    let lead = at(0);
    match bytes.len() {
        2 => ((lead & 0x1f) << 6) | (at(1) & 0x3f),
        3 => ((lead & 0x0f) << 12) | ((at(1) & 0x3f) << 6) | (at(2) & 0x3f),
        _ => {
            ((lead & 0x07) << 18) | ((at(1) & 0x3f) << 12) | ((at(2) & 0x3f) << 6) | (at(3) & 0x3f)
        }
    }
}
