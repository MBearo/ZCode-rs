// 复制自 apps/zcode-cli-rust/crates/model/src/sse.rs（feat/rs 79b6691c55），补上 `event:` 字段。
//! SSE 解码（WHATWG `text/event-stream`）。
//!
//! 输入是任意切分的字节块，输出是完整的事件。每个输入字节只扫描、复制一次；行与事件不限长
//! （一次写 300 KB 文件的工具调用参数就在一条 `data:` 行里）。
//!
//! 只处理 `event:` 与 `data:` 两个字段：`id:`、`retry:` 与 `:` 开头的注释行被忽略。空行结束一个
//! 事件；没有 `data:` 行的事件不分发。流在事件中途结束时，未完成的事件被丢弃（与规范一致）。

/// 一个完整的 SSE 事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// `event:` 字段；没有时为空（规范里的默认类型 `message`）。
    pub event: Option<String>,
    /// 多条 `data:` 行用 `\n` 连接。
    pub data: String,
}

/// SSE 字节流不是合法的 UTF-8。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("SSE stream is not valid UTF-8")]
pub struct SseError;

#[derive(Debug, Default)]
pub struct SseDecoder {
    line: Vec<u8>,
    event: Option<String>,
    data: String,
    has_data: bool,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 送入下一块字节，返回这块字节里完成的事件（可能为空，也可能有多个）。
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, SseError> {
        let mut events = Vec::new();
        for fragment in bytes.split_inclusive(|byte| *byte == b'\n') {
            self.line.extend_from_slice(fragment);
            if fragment.last() != Some(&b'\n') {
                continue;
            }
            let line = std::str::from_utf8(&self.line)
                .map_err(|_| SseError)?
                .trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if self.has_data {
                    events.push(SseEvent {
                        event: self.event.take(),
                        data: std::mem::take(&mut self.data),
                    });
                    self.has_data = false;
                }
                self.event = None;
            } else if let Some(data) = field(line, "data") {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(data);
                self.has_data = true;
            } else if let Some(event) = field(line, "event") {
                self.event = Some(event.to_owned());
            }
            self.line.clear();
        }
        Ok(events)
    }
}

/// `name:value` 或 `name: value` 的值（冒号后的第一个空格不属于值）。
fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let value = line.strip_prefix(name)?.strip_prefix(':')?;
    Some(value.strip_prefix(' ').unwrap_or(value))
}

#[cfg(test)]
#[path = "sse_tests.rs"]
mod tests;
