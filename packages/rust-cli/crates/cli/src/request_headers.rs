//! 模型请求的来源头与每请求头的 ID（工作线 F64，docs/specs/rust-cli/model.md §2.4）：入口从环境变量快照与系统读出
//! 事实，交给 model 的 `source_headers` 组装；trace ID 与 `x-request-id` 取自 `NodeIds`（ID 只有一个来源，README §4）。
//!
//! locale 与时区照 Node 的 ICU 读法（实测 Node 24 / ICU 78），只做 spec 写明的子集。

use std::sync::Arc;

use rust_cli_base::js::{node_arch, node_platform};
use rust_cli_model::{RequestHeaders, SourceFacts};
use rust_cli_node_store::NodeIds;

/// 系统给的两项：`os.release()` 与 `/etc/localtime` 的链接目标（Unix）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Host {
    pub(crate) os_release: Option<String>,
    pub(crate) localtime: Option<String>,
}

/// 入口用：读系统、算来源头，配上 `NodeIds` 的 trace ID 与 `x-request-id`。`var` 是快照里的原值（设了但为空时是
/// `Some("")`）；版本是本工程的 `VERSION`。ZCode 服务的地址无效时报错（启动失败，Node 建 App 时同样抛错）。
pub(crate) fn request_headers(
    var: &dyn Fn(&str) -> Option<String>,
    version: &str,
    ids: Arc<NodeIds>,
) -> Result<RequestHeaders, String> {
    let facts = facts(var, version, &host())?;
    let trace_id = ids.uuid();
    Ok(RequestHeaders {
        source: rust_cli_model::source_headers(&facts),
        trace_id,
        request_id: Arc::new(move || ids.uuid()),
    })
}

/// 来源头的输入。`var` 同上。
pub(crate) fn facts(
    var: &dyn Fn(&str) -> Option<String>,
    version: &str,
    host: &Host,
) -> Result<SourceFacts, String> {
    let non_empty = |name: &str| {
        var(name)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    };
    let test = crate::origins::is_test(&non_empty);
    Ok(SourceFacts {
        // Node `env[ZCODE_APP_VERSION] ?? appVersion`：设了（空值也算）就用它。
        version: var("ZCODE_APP_VERSION").unwrap_or_else(|| version.to_owned()),
        endpoint_origin: crate::origins::zcode_origin(&non_empty)?,
        release_channel: if test { "test" } else { "production" }.to_owned(),
        locale: if cfg!(windows) {
            None
        } else {
            Some(client_language(var))
        },
        time_zone: if cfg!(windows) {
            None
        } else {
            client_time_zone(var("TZ").as_deref(), host.localtime.as_deref())
        },
        platform: node_platform().to_owned(),
        arch: node_arch().to_owned(),
        os_release: host.os_release.clone(),
    })
}

/// ICU 的旧语言代码（`uloc` 的规范化）。
const LANGUAGE_ALIASES: [(&str, &str); 5] = [
    ("iw", "he"),
    ("in", "id"),
    ("ji", "yi"),
    ("jw", "jv"),
    ("mo", "ro"),
];

/// Node `Intl.DateTimeFormat().resolvedOptions().locale` 在 Unix 上的值（model.md §2.4 的读法）：依次看 `LC_ALL`、
/// `LC_MESSAGES`、`LANG`（空值也算设了）。
pub(crate) fn client_language(var: &dyn Fn(&str) -> Option<String>) -> String {
    let Some(id) = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|name| var(name))
    else {
        return "en-US".to_owned();
    };
    if id == "C" || id == "POSIX" {
        return "en-US".to_owned();
    }
    let base = id.split(['.', '@']).next().unwrap_or_default();
    // `@` 后的修饰（到 `.` 为止）是一个变体；`nynorsk` 写成 `ny`。
    let modifier = id
        .rsplit_once('@')
        .map(|(_, rest)| rest.split('.').next().unwrap_or_default())
        .map(|modifier| {
            if modifier == "nynorsk" {
                "ny"
            } else {
                modifier
            }
        });
    match base {
        // V8：ICU 名字是 `c` 的缺省 locale 也是 `en-US`。
        "C" | "POSIX" | "c" => return "en-US".to_owned(),
        "" if modifier.is_none() => return "und".to_owned(),
        _ => {}
    }
    let mut parts = base.split(['_', '-']);
    let language = parts.next().unwrap_or_default().to_ascii_lowercase();
    let language = if is_language(&language) {
        LANGUAGE_ALIASES
            .iter()
            .find(|(old, _)| *old == language)
            .map_or(language.clone(), |(_, new)| (*new).to_owned())
    } else {
        "und".to_owned()
    };
    let mut rest: Vec<&str> = parts.collect();
    let mut tag = vec![language];
    if let Some(script) = rest.first().copied().filter(|part| is_script(part)) {
        tag.push(titlecase(script));
        rest.remove(0);
    }
    if let Some(region) = rest.first().copied().filter(|part| is_region(part)) {
        tag.push(region.to_ascii_uppercase());
        rest.remove(0);
    }
    let mut variants = Vec::new();
    let mut private = Vec::new();
    let mut posix = false;
    for variant in rest
        .into_iter()
        .chain(modifier)
        .filter(|part| !part.is_empty())
    {
        let variant = variant.to_ascii_lowercase();
        if variant == "posix" {
            posix = true;
        } else if is_variant(&variant) {
            variants.push(variant);
        } else {
            private.push(variant);
        }
    }
    tag.extend(variants);
    if posix {
        // ICU 的 `en_US_POSIX` 是 V8 的缺省 locale `en-US`；其他的写成 Unicode 扩展。
        if tag == ["en", "US"] && private.is_empty() {
            return "en-US".to_owned();
        }
        tag.extend(["u".to_owned(), "va".to_owned(), "posix".to_owned()]);
    }
    if !private.is_empty() {
        tag.extend(["x".to_owned(), "lvariant".to_owned()]);
        tag.extend(private);
    }
    tag.join("-")
}

/// 语言子标签：2–3 或 5–8 个 ASCII 字母（`root` 是 `und`）。
fn is_language(value: &str) -> bool {
    matches!(value.len(), 2..=3 | 5..=8)
        && value.bytes().all(|byte| byte.is_ascii_alphabetic())
        && value != "root"
}

fn is_script(value: &str) -> bool {
    value.len() == 4 && value.bytes().all(|byte| byte.is_ascii_alphabetic())
}

fn is_region(value: &str) -> bool {
    (value.len() == 2 && value.bytes().all(|byte| byte.is_ascii_alphabetic()))
        || (value.len() == 3 && value.bytes().all(|byte| byte.is_ascii_digit()))
}

/// BCP 47 的变体：5–8 位字母数字，或数字开头的 4 位。
fn is_variant(value: &str) -> bool {
    let alnum = value.bytes().all(|byte| byte.is_ascii_alphanumeric());
    alnum
        && (matches!(value.len(), 5..=8)
            || (value.len() == 4 && value.as_bytes()[0].is_ascii_digit()))
}

fn titlecase(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    let mut chars = lower.chars();
    chars
        .next()
        .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
        .unwrap_or_default()
}

/// ICU 认作 UTC 的写法（Node 写 `UTC`）。
const UTC_NAMES: [&str; 17] = [
    "UTC",
    "Etc/UTC",
    "UCT",
    "Etc/UCT",
    "Universal",
    "Etc/Universal",
    "Zulu",
    "Etc/Zulu",
    "GMT0",
    "GMT+0",
    "GMT-0",
    "Etc/GMT",
    "Etc/GMT0",
    "Etc/GMT+0",
    "Etc/GMT-0",
    "Greenwich",
    "Etc/Greenwich",
];

/// CLDR 保留的旧名（ICU 的规范 ID；tzdb 改过名的时区，Node 写旧名）。
const CLDR_NAMES: [(&str, &str); 15] = [
    ("Asia/Kolkata", "Asia/Calcutta"),
    ("Asia/Ho_Chi_Minh", "Asia/Saigon"),
    ("Asia/Kathmandu", "Asia/Katmandu"),
    ("Asia/Yangon", "Asia/Rangoon"),
    ("Europe/Kyiv", "Europe/Kiev"),
    ("America/Nuuk", "America/Godthab"),
    ("Atlantic/Faroe", "Atlantic/Faeroe"),
    ("Pacific/Chuuk", "Pacific/Truk"),
    ("Pacific/Pohnpei", "Pacific/Ponape"),
    ("Pacific/Kanton", "Pacific/Enderbury"),
    ("America/Atikokan", "America/Coral_Harbour"),
    ("America/Argentina/Buenos_Aires", "America/Buenos_Aires"),
    ("America/Indiana/Indianapolis", "America/Indianapolis"),
    ("America/Kentucky/Louisville", "America/Louisville"),
    ("Africa/Asmara", "Africa/Asmera"),
];

/// Node `Intl.DateTimeFormat().resolvedOptions().timeZone` 在 Unix 上的值（model.md §2.4 的读法）。`tz` 是 `TZ` 的原值，
/// `localtime` 是 `/etc/localtime` 的链接目标；None 时 Node 写 `unknown`。
pub(crate) fn client_time_zone(tz: Option<&str>, localtime: Option<&str>) -> Option<String> {
    let name = match tz {
        Some(tz) => {
            let name = tz.strip_prefix(':').unwrap_or(tz);
            if name.is_empty() {
                return Some("Etc/Unknown".to_owned());
            }
            if UTC_NAMES.contains(&name) {
                return Some("UTC".to_owned());
            }
            if name == "GMT" {
                return Some("+00:00".to_owned());
            }
            let allowed = |byte: u8| byte.is_ascii_alphanumeric() || b"/_+-".contains(&byte);
            if name.starts_with('/') || !name.bytes().all(allowed) {
                return None;
            }
            name.to_owned()
        }
        None => localtime?.rsplit_once("zoneinfo/")?.1.to_owned(),
    };
    Some(
        CLDR_NAMES
            .iter()
            .find(|(new, _)| *new == name)
            .map_or(name.clone(), |(_, old)| (*old).to_owned()),
    )
}

/// 系统给的两项：`os.release()`（三个平台，cli `os_release`）与 `/etc/localtime` 的链接目标（Unix）。读不到的为 None。
fn host() -> Host {
    Host {
        os_release: crate::os_release::node_release(),
        localtime: localtime_link(),
    }
}

/// Windows 没有这个文件（时区写 `unknown`，未做）。
#[cfg(not(unix))]
fn localtime_link() -> Option<String> {
    None
}

#[cfg(unix)]
#[expect(
    clippy::disallowed_methods,
    reason = "入口读一次系统时区的名字（/etc/localtime 的链接，模型请求的 x-client-timezone，model.md §2.4）"
)]
fn localtime_link() -> Option<String> {
    std::fs::read_link("/etc/localtime")
        .ok()
        .map(|target| target.to_string_lossy().into_owned())
}

#[cfg(test)]
#[path = "request_headers_tests.rs"]
mod tests;
