//! 系统时区（docs/specs/rust-cli/time-zone.md §1）：入口启动时读一次，交给 runtime 的 [`SystemClock`]。
//!
//! Unix 用 libc（`localtime_r` 读 `TZ`，没有时读 `/etc/localtime`；偏移是 `tm_gmtoff`），Windows
//! 用 `GetDynamicTimeZoneInformation` 与 `SystemTimeToTzSpecificLocalTimeEx`。偏移按每个时刻求，夏令时与
//! JS `Date` 的本地取值一致。`TZ` 是 UTC 写法时三个平台都直接按 UTC（测试与 CI 用它）。
//!
//! [`SystemClock`]: rust_cli_node_store::SystemClock

use std::sync::Arc;

use rust_cli_node_store::{FixedOffset, TimeZone};

/// 按 `TZ` 与系统设置给出时区。
pub(crate) fn system(tz: Option<&str>) -> Arc<dyn TimeZone> {
    let tz = tz.map(str::trim).filter(|value| !value.is_empty());
    match tz {
        Some(value) if is_utc(value) => Arc::new(FixedOffset::new(name(value), 0)),
        _ => platform::load(tz.map(name)),
    }
}

/// `TZ` 的名字：去掉开头的 `:`；时区文件的绝对路径取 `zoneinfo/` 之后的部分。
pub(crate) fn name(tz: &str) -> String {
    let tz = tz.trim().trim_start_matches(':');
    match tz.rsplit_once("zoneinfo/") {
        Some((_, zone)) if tz.starts_with('/') => zone.to_owned(),
        _ => tz.to_owned(),
    }
}

/// UTC 的写法。
pub(crate) fn is_utc(tz: &str) -> bool {
    const UTC: &[&str] = &[
        "UTC",
        "UCT",
        "GMT",
        "UTC0",
        "GMT0",
        "GMT+0",
        "GMT-0",
        "Universal",
        "Zulu",
        "Etc/UTC",
        "Etc/UCT",
        "Etc/GMT",
        "Etc/GMT0",
        "Etc/GMT+0",
        "Etc/GMT-0",
        "Etc/Universal",
        "Etc/Zulu",
    ];
    UTC.contains(&name(tz).as_str())
}

#[cfg(unix)]
mod platform {
    use std::mem::MaybeUninit;
    use std::sync::Arc;

    use rust_cli_base::units;
    use rust_cli_node_store::TimeZone;

    /// `tm_gmtoff`（秒）换成分钟。
    const SECONDS_PER_MINUTE: libc::c_long = units::SECONDS_PER_MINUTE as libc::c_long;

    /// libc 的本地时区（`TZ` 或 `/etc/localtime`）。
    struct LibcZone {
        name: String,
    }

    impl TimeZone for LibcZone {
        fn name(&self) -> String {
            self.name.clone()
        }

        fn offset_minutes(&self, at_ms: u64) -> i32 {
            offset(at_ms).unwrap_or(0)
        }
    }

    pub(super) fn load(name: Option<String>) -> Arc<dyn TimeZone> {
        // libc 在第一次 localtime_r 时读时区（libc crate 在 Unix 上没有导出 tzset）：启动时先调一次。
        let _ = offset(0);
        Arc::new(LibcZone {
            name: name.unwrap_or_else(|| "localtime".to_owned()),
        })
    }

    #[expect(unsafe_code, reason = "localtime_r(3) 是 libc 的 FFI，没有安全封装")]
    fn offset(at_ms: u64) -> Option<i32> {
        // 缺陷修复（F41，linux.md §4.1）：原来写 `libc::time_t::try_from`，musl 上 libc crate 把 `time_t`
        // 标成 deprecated（预告 32 位 musl 改成 64 位），Linux musl 的 clippy -D warnings 失败。这里不写出类型名，
        // 由 localtime_r 的参数推断，各平台仍是 time_t，行为不变。
        let seconds = (at_ms / units::MS_PER_SECOND).try_into().ok()?;
        let mut parts = MaybeUninit::<libc::tm>::uninit();
        // SAFETY: localtime_r 是可重入版本，只写第二个参数；两个指针都指向本函数里有效的内存。
        let filled = unsafe { libc::localtime_r(&seconds, parts.as_mut_ptr()) };
        if filled.is_null() {
            return None;
        }
        // SAFETY: 返回非空表示 `parts` 已写满。
        let parts = unsafe { parts.assume_init() };
        i32::try_from(parts.tm_gmtoff / SECONDS_PER_MINUTE).ok()
    }
}

#[cfg(windows)]
mod platform {
    use std::sync::Arc;

    use rust_cli_base::units;
    use rust_cli_node_store::{FixedOffset, TimeZone};
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{
        DYNAMIC_TIME_ZONE_INFORMATION, FileTimeToSystemTime, GetDynamicTimeZoneInformation,
        SystemTimeToFileTime, SystemTimeToTzSpecificLocalTimeEx, TIME_ZONE_ID_INVALID,
    };

    /// 1601-01-01 到 1970-01-01 的 100 纳秒数（FILETIME 的起点）。
    const EPOCH_TICKS: u64 = units::FILETIME_EPOCH_OFFSET_MS * units::FILETIME_TICKS_PER_MS;
    /// 一分钟的 100 纳秒数。
    const TICKS_PER_MINUTE: i64 = (units::MS_PER_MINUTE * units::FILETIME_TICKS_PER_MS) as i64;

    /// 系统的时区（`TZ` 不是 UTC 写法时也用它，time-zone.md D1）。
    struct SystemZone {
        info: DYNAMIC_TIME_ZONE_INFORMATION,
        name: String,
    }

    impl TimeZone for SystemZone {
        fn name(&self) -> String {
            self.name.clone()
        }

        fn offset_minutes(&self, at_ms: u64) -> i32 {
            offset(&self.info, at_ms).unwrap_or(0)
        }
    }

    #[expect(
        unsafe_code,
        reason = "GetDynamicTimeZoneInformation 是 Win32 FFI，没有安全封装"
    )]
    pub(super) fn load(_name: Option<String>) -> Arc<dyn TimeZone> {
        let mut info = DYNAMIC_TIME_ZONE_INFORMATION::default();
        // SAFETY: 只写 `info`，指针指向本函数里有效的内存。
        if unsafe { GetDynamicTimeZoneInformation(&mut info) } == TIME_ZONE_ID_INVALID {
            tracing::warn!("system time zone unavailable, using UTC");
            return Arc::new(FixedOffset::utc());
        }
        let key = &info.TimeZoneKeyName;
        let end = key.iter().position(|unit| *unit == 0).unwrap_or(key.len());
        let name = String::from_utf16_lossy(&key[..end]);
        Arc::new(SystemZone { info, name })
    }

    fn ticks(file: FILETIME) -> u64 {
        (u64::from(file.dwHighDateTime) << 32) | u64::from(file.dwLowDateTime)
    }

    #[expect(unsafe_code, reason = "Win32 时间换算 API 是 FFI，没有安全封装")]
    fn offset(info: &DYNAMIC_TIME_ZONE_INFORMATION, at_ms: u64) -> Option<i32> {
        let at = at_ms
            .checked_mul(units::FILETIME_TICKS_PER_MS)?
            .checked_add(EPOCH_TICKS)?;
        let utc_file = FILETIME {
            dwLowDateTime: u32::try_from(at & u64::from(u32::MAX)).ok()?,
            dwHighDateTime: u32::try_from(at >> 32).ok()?,
        };
        let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
        let mut local_file = FILETIME::default();
        // SAFETY: 三个调用都只读前面的参数、只写最后一个参数，指针都指向本函数里有效的内存。
        let converted = unsafe {
            FileTimeToSystemTime(&utc_file, &mut utc) != 0
                && SystemTimeToTzSpecificLocalTimeEx(info, &utc, &mut local) != 0
                && SystemTimeToFileTime(&local, &mut local_file) != 0
        };
        if !converted {
            return None;
        }
        let shift = i64::try_from(ticks(local_file)).ok()? - i64::try_from(at).ok()?;
        i32::try_from(shift.div_euclid(TICKS_PER_MINUTE)).ok()
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use std::sync::Arc;

    use rust_cli_node_store::{FixedOffset, TimeZone};

    pub(super) fn load(_name: Option<String>) -> Arc<dyn TimeZone> {
        Arc::new(FixedOffset::utc())
    }
}

#[cfg(test)]
#[path = "time_zone_tests.rs"]
mod tests;
