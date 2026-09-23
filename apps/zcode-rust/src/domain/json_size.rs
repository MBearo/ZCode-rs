use serde::Serialize;
use std::io::Write;

#[derive(Default)]
struct Counter(usize);
impl Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(super) fn serialized_size(value: &impl Serialize) -> serde_json::Result<usize> {
    let mut counter = Counter::default();
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.0)
}
