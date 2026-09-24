//! Process liveness and start times for the trust store lock (Node
//! `os.uptime()`, `process.kill(pid, 0)` and the start-time probes).

#[cfg(target_os = "macos")]
fn uptime_seconds() -> Option<f64> {
    let mut boot = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut size = std::mem::size_of::<libc::timeval>();
    let mut mib = [libc::CTL_KERN, libc::KERN_BOOTTIME];
    // SAFETY: mib 与输出缓冲区大小都由调用方给出，sysctl 只写入 timeval 大小的数据。
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            2,
            (&mut boot as *mut libc::timeval).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    // libuv uv_uptime 在 macOS 上是整秒：now - boottime.tv_sec。
    Some(now.saturating_sub(boot.tv_sec as u64) as f64)
}

#[cfg(target_os = "linux")]
fn uptime_seconds() -> Option<f64> {
    // SAFETY: sysinfo 只写入传入的结构体。
    let mut info: libc::sysinfo = unsafe { std::mem::zeroed() };
    (unsafe { libc::sysinfo(&mut info) } == 0).then_some(info.uptime as f64)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn uptime_seconds() -> Option<f64> {
    None
}

/// `kill(pid, 0)`: EPERM still means alive. Other platforms cannot probe and
/// treat an over-age lock as abandoned.
pub(crate) fn alive(pid: i64) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        // SAFETY: 信号 0 只做存在性探测，不影响目标进程。
        let signalled = unsafe { libc::kill(pid, 0) } == 0;
        signalled || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Node `currentProcessStartTimeMs` = `Date.now() - os.uptime() * 1000`, which
/// is the machine's boot time rather than the process start (D15, kept).
pub(crate) fn boot_time_ms() -> i64 {
    static BOOT: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *BOOT.get_or_init(|| crate::now() as i64 - uptime_seconds().map_or(0, |s| (s * 1000.0) as i64))
}

/// Node `probeProcessStartTimeDefault` (wall-clock ms), `None` when unknown.
pub(crate) async fn process_start_ms(pid: i64) -> Option<i64> {
    if cfg!(target_os = "linux") {
        let stat = tokio::fs::read_to_string(format!("/proc/{pid}/stat"))
            .await
            .ok()?;
        let rest = &stat[stat.rfind(')')? + 2..];
        let ticks: f64 = rest.split(' ').nth(19)?.parse().ok()?;
        return Some(boot_time_ms() + (ticks * 1000.0 / 100.0) as i64);
    }
    if cfg!(target_os = "macos") {
        let output = tokio::process::Command::new("ps")
            .args(["-o", "lstart=", "-p", &pid.to_string()])
            .env("LC_ALL", "C")
            .kill_on_drop(true)
            .output()
            .await
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let parsed =
            chrono::NaiveDateTime::parse_from_str(text.trim(), "%a %b %e %H:%M:%S %Y").ok()?;
        let local = parsed.and_local_timezone(chrono::Local).earliest()?;
        return Some(local.timestamp_millis());
    }
    None
}
