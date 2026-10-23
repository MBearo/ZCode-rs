//! 文本文件的编码与换行。
//!
//! 读：开头是 `FF FE` 按 UTF-16LE，否则按 UTF-8（有损）；去掉 BOM，`\r\n` 换成 `\n`；
//! 换行风格按前 4096 个字符里 CRLF 与 LF 的多少决定。写：按原编码编码，CRLF 文件把 `\n` 换回
//! `\r\n`。写回时不再加 BOM。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Encoding {
    Utf8,
    Utf16Le,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineEndings {
    Lf,
    Crlf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Decoded {
    /// 去掉 BOM、换行归一后的内容。
    pub content: String,
    pub encoding: Encoding,
    pub line_endings: LineEndings,
}

fn line_endings(text: &str) -> LineEndings {
    let (mut crlf, mut lf) = (0, 0);
    let mut previous = None;
    for c in text.chars().take(4096) {
        if c == '\n' {
            if previous == Some('\r') {
                crlf += 1;
            } else {
                lf += 1;
            }
        }
        previous = Some(c);
    }
    if crlf > lf {
        LineEndings::Crlf
    } else {
        LineEndings::Lf
    }
}

/// 去掉 BOM、CRLF 换成 LF。
pub(crate) fn normalize(text: &str) -> String {
    text.strip_prefix('\u{feff}')
        .unwrap_or(text)
        .replace("\r\n", "\n")
}

pub(crate) fn decode(bytes: &[u8]) -> Decoded {
    let (text, encoding) = if bytes.starts_with(&[0xff, 0xfe]) {
        let units: Vec<u16> = bytes
            .chunks(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair.get(1).copied().unwrap_or(0)]))
            .collect();
        (String::from_utf16_lossy(&units), Encoding::Utf16Le)
    } else {
        (String::from_utf8_lossy(bytes).into_owned(), Encoding::Utf8)
    };
    Decoded {
        line_endings: line_endings(&text),
        content: normalize(&text),
        encoding,
    }
}

pub(crate) fn encode(content: &str, encoding: Encoding, endings: LineEndings) -> Vec<u8> {
    let text = match endings {
        LineEndings::Lf => content.to_owned(),
        LineEndings::Crlf => content.replace("\r\n", "\n").replace('\n', "\r\n"),
    };
    match encoding {
        Encoding::Utf8 => text.into_bytes(),
        Encoding::Utf16Le => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
    }
}
