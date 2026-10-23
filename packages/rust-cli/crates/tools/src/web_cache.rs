//! WebFetch 的页面缓存（LRU、50MB、按 URL、15 分钟过期）。在工具实例里（每个内核一份），不是进程级全局。
//!
//! 时间取自注入的 Clock（F7，审查 R1-15：以前用 tokio 的 `Instant`，内核 crate 不碰系统时钟，README §4）。

use std::collections::VecDeque;

/// 过期时间（15 分钟）。
const TTL_MS: u64 = 15 * 60 * 1000;
/// 缓存的正文总量上限（50MB）。
const MAX_BYTES: usize = 50 * 1024 * 1024;

/// 取到的页面：转换后的正文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Page {
    pub(crate) code: u16,
    pub(crate) content: String,
    pub(crate) content_type: String,
}

#[derive(Default)]
pub(crate) struct PageCache {
    /// 最近用过的在后；中间是存入时的 Unix 毫秒。
    entries: VecDeque<(String, u64, Page)>,
    bytes: usize,
}

impl PageCache {
    fn remove(&mut self, url: &str) -> Option<(String, u64, Page)> {
        let index = self.entries.iter().position(|(key, _, _)| key == url)?;
        let entry = self.entries.remove(index)?;
        self.bytes -= entry.2.content.len();
        Some(entry)
    }

    pub(crate) fn get(&mut self, url: &str, now_ms: u64) -> Option<Page> {
        let (key, at, page) = self.remove(url)?;
        if now_ms.saturating_sub(at) >= TTL_MS {
            return None;
        }
        self.bytes += page.content.len();
        self.entries.push_back((key, at, page.clone()));
        Some(page)
    }

    pub(crate) fn put(&mut self, url: &str, page: Page, now_ms: u64) {
        self.remove(url);
        self.bytes += page.content.len();
        self.entries.push_back((url.to_owned(), now_ms, page));
        while self.bytes > MAX_BYTES {
            let Some((_, _, evicted)) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= evicted.content.len();
        }
    }
}

#[cfg(test)]
#[path = "web_cache_tests.rs"]
mod tests;
