//! Host facts reported in identity headers, named the way Node reports them
//! (`Intl` locale and time zone, `process.platform`, `os.arch()`, `os.release()`).
use std::path::Path;

/// ICU default locale as BCP 47: the first *present* of `LC_ALL`, `LC_MESSAGES`,
/// `LANG`; `C`/`POSIX` and absence map to `en-US`, an empty value to `und`.
pub fn language<'a>(env: impl Fn(&str) -> Option<&'a str>) -> String {
    let Some(raw) = ["LC_ALL", "LC_MESSAGES", "LANG"].into_iter().find_map(&env) else {
        return "en-US".into();
    };
    let (locale, modifier) = match raw.split_once('@') {
        Some((locale, modifier)) => (locale, Some(modifier)),
        None => (raw, None),
    };
    let locale = locale.split('.').next().unwrap_or("");
    match locale {
        "C" | "POSIX" => return "en-US".into(),
        "" => return "und".into(),
        _ => {}
    }
    let mut tag = locale.replace('_', "-");
    if let Some(modifier) = modifier.filter(|m| !m.is_empty()) {
        tag.push('-');
        tag.push_str(modifier);
    }
    tag
}

/// IANA time zone like `Intl.DateTimeFormat().resolvedOptions().timeZone`.
/// `None` when `TZ` names an unknown zone (Node reports `undefined`).
pub fn timezone<'a>(env: impl Fn(&str) -> Option<&'a str>) -> Option<String> {
    let Some(tz) = env("TZ") else {
        return iana_time_zone::get_timezone().ok();
    };
    let tz = tz.strip_prefix(':').unwrap_or(tz);
    if tz.is_empty() {
        return Some("Etc/Unknown".into());
    }
    let known = tz.eq_ignore_ascii_case("UTC")
        || (!tz.contains("..") && Path::new("/usr/share/zoneinfo").join(tz).is_file());
    known.then(|| tz.to_owned())
}

pub struct Os {
    pub platform: &'static str,
    pub arch: &'static str,
    pub category: &'static str,
    pub release: Option<String>,
}

/// `process.platform`, `os.arch()`, `os.release()` and the Node OS category.
pub fn os() -> Os {
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        "powerpc64" => "ppc64",
        other => other,
    };
    let category = match platform {
        "darwin" => "macos",
        "win32" => "windows",
        _ => "linux",
    };
    Os {
        platform,
        arch,
        category,
        release: release(),
    }
}

#[cfg(unix)]
fn release() -> Option<String> {
    // SAFETY: uname 只写入调用方提供的结构体；失败时不读取其内容。
    let mut name: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut name) } != 0 {
        return None;
    }
    let bytes: Vec<u8> = name
        .release
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8(bytes).ok()
}

#[cfg(windows)]
fn release() -> Option<String> {
    use windows_sys::{
        Wdk::System::SystemServices::RtlGetVersion,
        Win32::System::SystemInformation::OSVERSIONINFOW,
    };
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    // SAFETY: RtlGetVersion 只写入已按大小初始化的结构体（libuv `uv_os_uname` 同样调用它）。
    if unsafe { RtlGetVersion(&mut info) } != 0 {
        return None;
    }
    Some(format!(
        "{}.{}.{}",
        info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber
    ))
}

#[cfg(not(any(unix, windows)))]
fn release() -> Option<String> {
    None
}
