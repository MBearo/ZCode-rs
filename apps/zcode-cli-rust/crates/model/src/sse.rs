use crate::{contract::ModelFailure, domain::MAX_TEXT_BYTES};

#[derive(Default)]
pub struct SseDecoder {
    line: Vec<u8>,
    data: String,
    has_data: bool,
}
impl SseDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, ModelFailure> {
        let mut events = Vec::new();
        // 每个输入 byte 只扫描/复制一次，不 drain 前缀，也不重复扫描残留长行。
        for fragment in bytes.split_inclusive(|b| *b == b'\n') {
            if self.line.len() + fragment.len() > MAX_TEXT_BYTES + 2 {
                return Err(ModelFailure::invalid());
            }
            self.line.extend_from_slice(fragment);
            if fragment.last() != Some(&b'\n') {
                continue;
            }
            let line = std::str::from_utf8(&self.line)
                .map_err(|_| ModelFailure::invalid())?
                .trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if self.has_data {
                    events.push(std::mem::take(&mut self.data));
                    self.has_data = false;
                }
            } else if let Some(data) = line.strip_prefix("data:") {
                let data = data.strip_prefix(' ').unwrap_or(data);
                if self.data.len() + data.len() + usize::from(self.has_data) > MAX_TEXT_BYTES {
                    return Err(ModelFailure::invalid());
                }
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(data);
                self.has_data = true;
            }
            self.line.clear();
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_multiline_crlf_and_large_multi_event_chunk() {
        let mut decoder = SseDecoder::default();
        let mut events = vec![];
        for byte in "data: 你好\r\ndata: world\r\n\r\n".as_bytes() {
            events.extend(decoder.push(&[*byte]).unwrap());
        }
        assert_eq!(events, vec!["你好\nworld"]);
        let frame = "data: {}\n\n";
        let events = decoder.push(frame.repeat(40_000).as_bytes()).unwrap();
        assert_eq!(events.len(), 40_000);
    }
    #[test]
    fn line_and_event_limits_apply_across_packets() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.push(&vec![b'a'; MAX_TEXT_BYTES + 3]).is_err());
        let mut decoder = SseDecoder::default();
        for _ in 0..4 {
            decoder
                .push(format!("data: {}\n", "x".repeat(60_000)).as_bytes())
                .unwrap();
        }
        assert!(
            decoder
                .push(format!("data: {}\n", "x".repeat(60_000)).as_bytes())
                .is_err()
        );
        assert!(SseDecoder::default().push(b"data: \xff\n\n").is_err());
    }
}
