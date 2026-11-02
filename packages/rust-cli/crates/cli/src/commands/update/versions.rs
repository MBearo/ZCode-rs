//! 版本比较（docs/specs/rust-cli/update.md §5.1）。

/// `latest` 是否比 `current` 新；任何一边不能解析时为 None（调用方当作不新）。
pub(crate) fn is_newer(latest: &str, current: &str) -> Option<bool> {
    match (parse(latest), parse(current)) {
        (Some(latest), Some(current)) => Some(latest > current),
        _ => None,
    }
}

/// 源码构建的版本（`0.0.0`，不检查更新）。
pub(crate) fn is_source_build(version: &str) -> bool {
    parse(version) == Some((0, 0, 0))
}

/// 前三段各按十进制无符号整数解析（先去掉首尾空白）；预发布的 `0-rc` 这类第三段解析失败。
fn parse(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim().split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

#[cfg(test)]
#[path = "versions_tests.rs"]
mod tests;
