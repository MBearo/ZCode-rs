//! 词的切分：Scala 用 `Intl.Segmenter`（granularity = word）；这里是自带的规则（grapheme 与中文分词）。
//!
//! 规则（UAX #29 的子集）：字母、数字与 `_` 连成一个词；字母之间的 `'` `’` `.` `:` `·` 与数字之间的 `,` `.` `;` 不断开；
//! 组合符号跟着前一个字；片假名连成一个词；汉字与平假名一个字一个词（ICU 的词典分词做不到，§15.1 登记）。其余每个码点
//! 自成一个不是词的段（连着的空格是一段）。位置是字节偏移。

/// 一段：`[start, end)` 与它是不是词。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub start: usize,
    pub end: usize,
    pub word: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Letter,
    Digit,
    Join,
    Ideo,
    Kana,
    MidLetter,
    MidNum,
    MidBoth,
    Extend,
    Space,
    Other,
}

fn class(c: char) -> Class {
    let u = c as u32;
    if c == '_' || c == '\u{203f}' || c == '\u{2040}' {
        Class::Join
    } else if c == ' ' || c == '\t' {
        Class::Space
    } else if matches!(
        c,
        '\'' | '.' | '\u{2019}' | '\u{2018}' | '\u{2024}' | '\u{fe52}' | '\u{ff07}' | '\u{ff0e}'
    ) {
        Class::MidBoth
    } else if matches!(
        c,
        ':' | '\u{b7}' | '\u{387}' | '\u{5f4}' | '\u{2027}' | '\u{fe13}' | '\u{fe55}' | '\u{ff1a}'
    ) {
        Class::MidLetter
    } else if matches!(
        c,
        ',' | ';'
            | '\u{37e}'
            | '\u{589}'
            | '\u{60c}'
            | '\u{60d}'
            | '\u{66c}'
            | '\u{7f8}'
            | '\u{2044}'
            | '\u{fe10}'
            | '\u{fe14}'
            | '\u{fe50}'
            | '\u{fe54}'
            | '\u{ff0c}'
            | '\u{ff1b}'
    ) {
        Class::MidNum
    } else if (0x3040..=0x309f).contains(&u) || is_ideo(u) {
        Class::Ideo
    } else if (0x30a0..=0x30ff).contains(&u)
        || (0x31f0..=0x31ff).contains(&u)
        || (0xff66..=0xff9f).contains(&u)
    {
        Class::Kana
    } else if is_extend(u) {
        Class::Extend
    } else if c.is_numeric() {
        Class::Digit
    } else if c.is_alphabetic() {
        Class::Letter
    } else {
        Class::Other
    }
}

fn is_ideo(u: u32) -> bool {
    (0x3400..=0x4dbf).contains(&u)
        || (0x4e00..=0x9fff).contains(&u)
        || (0xf900..=0xfaff).contains(&u)
        || (0x20000..=0x3134f).contains(&u)
        || u == 0x3005
        || u == 0x3007
}

fn is_extend(u: u32) -> bool {
    (0x300..=0x36f).contains(&u)
        || (0x1ab0..=0x1aff).contains(&u)
        || (0x1dc0..=0x1dff).contains(&u)
        || (0x20d0..=0x20ff).contains(&u)
        || (0xfe00..=0xfe0f).contains(&u)
        || (0xfe20..=0xfe2f).contains(&u)
        || u == 0x200d
        || (0xe0100..=0xe01ef).contains(&u)
}

fn wordish(c: Class) -> bool {
    matches!(c, Class::Letter | Class::Digit | Class::Join)
}

/// `text` 切成的段，按次序，首尾相接。
pub fn segments(text: &str) -> Vec<Segment> {
    let chars: Vec<(usize, char, Class)> =
        text.char_indices().map(|(i, c)| (i, c, class(c))).collect();
    let end_of = |k: usize| chars.get(k).map_or(text.len(), |c| c.0);
    let mut out = Vec::new();
    let mut k = 0;
    while k < chars.len() {
        let start = chars[k].0;
        let first = chars[k].2;
        let mut j = k + 1;
        let word = match first {
            Class::Letter | Class::Digit | Class::Join => {
                // 上一个算数的字（组合符号不算）。
                let mut last = first;
                while j < chars.len() {
                    let c = chars[j].2;
                    if c == Class::Extend || wordish(c) {
                        if c != Class::Extend {
                            last = c;
                        }
                        j += 1;
                        continue;
                    }
                    // 中间的标点：两边同类才不断开。
                    let next = chars.get(j + 1).map(|n| n.2);
                    let letters = last == Class::Letter && next == Some(Class::Letter);
                    let digits = last == Class::Digit && next == Some(Class::Digit);
                    let mid = match c {
                        Class::MidLetter => letters,
                        Class::MidNum => digits,
                        Class::MidBoth => letters || digits,
                        _ => false,
                    };
                    if mid {
                        j += 2;
                        last = next.unwrap_or(last);
                    } else {
                        break;
                    }
                }
                true
            }
            Class::Kana => {
                while j < chars.len() && matches!(chars[j].2, Class::Kana | Class::Extend) {
                    j += 1;
                }
                true
            }
            Class::Ideo => {
                while j < chars.len() && chars[j].2 == Class::Extend {
                    j += 1;
                }
                true
            }
            Class::Space => {
                while j < chars.len() && matches!(chars[j].2, Class::Space | Class::Extend) {
                    j += 1;
                }
                false
            }
            _ => {
                while j < chars.len() && chars[j].2 == Class::Extend {
                    j += 1;
                }
                false
            }
        };
        out.push(Segment {
            start,
            end: end_of(j),
            word,
        });
        k = j;
    }
    out
}
