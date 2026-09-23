use super::json_size::serialized_size;
use super::session::Session;
use serde_json::Value;
use std::mem::size_of;

fn heap_bytes(value: &Value) -> usize {
    match value {
        Value::String(s) => s.capacity(),
        Value::Array(values) => {
            values.capacity() * size_of::<Value>() + values.iter().map(heap_bytes).sum::<usize>()
        }
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| {
                // BTreeMap 的节点和空槽按每项额外四个指针估计；预算不冒充分配器 RSS。
                size_of::<(String, Value)>()
                    + 4 * size_of::<usize>()
                    + key.capacity()
                    + heap_bytes(value)
            })
            .sum(),
        _ => 0,
    }
}
impl Session {
    pub fn estimated_resident_bytes(&mut self) -> usize {
        if let Some(bytes) = self.resident_bytes {
            return bytes;
        }
        // 只在会话空闲且事实变更后计算；不为估算生成另一份历史 JSON 或逐 chunk 扫描。
        let bytes = size_of::<Self>()
            + (self.rows.capacity() + self.messages.capacity()) * size_of::<Value>()
            + self
                .rows
                .iter()
                .chain(&self.messages)
                .map(heap_bytes)
                .sum::<usize>()
            + 2 * (serialized_size(self).expect("Session metadata is serializable")
                + serialized_size(&self.history).expect("History is serializable"))
            + self.history.action_rows.capacity() * size_of::<usize>();
        self.resident_bytes = Some(bytes);
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_capacity_is_counted_without_serialized_history_allocation() {
        let mut s = Session::new(
            "s".into(),
            "w".into(),
            "p".into(),
            "m".into(),
            "none".into(),
            "e".into(),
            0,
        );
        let empty = s.estimated_resident_bytes();
        let mut text = String::with_capacity(1024 * 1024);
        text.push('x');
        s.messages.push(Value::String(text));
        s.resident_bytes = None;
        assert!(s.estimated_resident_bytes() >= empty + 1024 * 1024);
        assert!(!serde_json::to_string(&s).unwrap().contains("residentBytes"));
    }
}
