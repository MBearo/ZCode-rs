//! Update detection of installed plugins (Node `version-compare.ts`).

/// `semver.coerce`: the first `X[.Y[.Z]]` of at most 16 digits per part.
fn coerce(text: &str) -> Option<(u64, u64, u64)> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() || (i > 0 && bytes[i - 1].is_ascii_digit()) {
            i += 1;
            continue;
        }
        let mut parts = vec![];
        let mut j = i;
        loop {
            let start = j;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j - start == 0 || j - start > 16 {
                break;
            }
            parts.push(text[start..j].parse::<u64>().ok()?);
            if parts.len() == 3 || j >= bytes.len() || bytes[j] != b'.' {
                break;
            }
            if !bytes.get(j + 1).is_some_and(u8::is_ascii_digit) {
                break;
            }
            j += 1;
        }
        // 版本号后紧跟数字（超过 16 位）时不匹配，从下一个位置继续。
        if !parts.is_empty() && !bytes.get(j).is_some_and(u8::is_ascii_digit) {
            return Some((
                parts[0],
                parts.get(1).copied().unwrap_or(0),
                parts.get(2).copied().unwrap_or(0),
            ));
        }
        i += 1;
    }
    None
}

/// Node `comparePluginVersions`.
pub fn compare_versions(installed: Option<&str>, latest: Option<&str>) -> &'static str {
    let (Some(installed), Some(latest)) = (
        installed.filter(|s| !s.is_empty()),
        latest.filter(|s| !s.is_empty()),
    ) else {
        return "none";
    };
    match (coerce(installed), coerce(latest)) {
        (Some(installed), Some(latest)) if latest > installed => "update-available",
        (Some(_), Some(_)) => "none",
        _ if installed == latest => "none",
        _ => "version-changed",
    }
}

/// Node `comparePluginUpdate`: the version axis when the catalog has one,
/// else the source identity pin.
pub fn compare_update(
    installed_version: Option<&str>,
    installed_sha: Option<&str>,
    latest_version: Option<&str>,
    latest_sha: Option<&str>,
) -> &'static str {
    if latest_version.is_some_and(|v| !v.is_empty()) {
        return compare_versions(installed_version, latest_version);
    }
    match (latest_sha.filter(|s| !s.is_empty()), installed_sha) {
        (None, _) => "none",
        (Some(_), None) => "version-changed",
        (Some(latest), Some(installed)) if latest == installed => "none",
        _ => "update-available",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_like_semver_coerce() {
        assert_eq!(coerce("v1.2"), Some((1, 2, 0)));
        assert_eq!(coerce("release-2.3.4-beta"), Some((2, 3, 4)));
        assert_eq!(coerce("abc"), None);
        assert_eq!(
            compare_versions(Some("1.0.0"), Some("1.0.1")),
            "update-available"
        );
        assert_eq!(compare_versions(Some("2.0.0"), Some("1.9.0")), "none");
        assert_eq!(
            compare_versions(Some("main"), Some("dev")),
            "version-changed"
        );
        assert_eq!(compare_versions(None, Some("1")), "none");
        assert_eq!(
            compare_update(Some("1"), None, None, Some("abc")),
            "version-changed"
        );
        assert_eq!(
            compare_update(Some("1"), Some("a"), None, Some("b")),
            "update-available"
        );
        assert_eq!(compare_update(Some("1"), Some("a"), None, None), "none");
    }
}
