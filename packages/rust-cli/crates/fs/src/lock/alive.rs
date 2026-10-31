// 复制自 apps/zcode-host/crates/base/src/store/process.rs（07ab266d4b）。
//! 进程存活判断，与 Node `process.kill(pid, 0)` 语义相同：能发信号或无权限都算存活。

#[cfg(unix)]
#[expect(unsafe_code, reason = "kill(2) 没有安全封装")]
pub(crate) fn is_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: kill(pid, 0) 不发送信号，只做存在性与权限检查，不读写本进程内存。
    let sent = unsafe { libc::kill(pid, 0) } == 0;
    sent || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
#[expect(unsafe_code, reason = "OpenProcess / GetExitCodeProcess 没有安全封装")]
pub(crate) fn is_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    /// Windows 的 `ERROR_ACCESS_DENIED`（系统错误码）。
    const ERROR_ACCESS_DENIED: i32 = 5;
    // SAFETY: 只在本函数内打开、查询并关闭同一个进程句柄，指针参数指向栈上变量。
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            // ERROR_ACCESS_DENIED：无权打开同样说明进程存在（Node 的 EPERM 语义）。
            return std::io::Error::last_os_error().raw_os_error() == Some(ERROR_ACCESS_DENIED);
        }
        let mut code = 0;
        let alive = GetExitCodeProcess(handle, &mut code) != 0 && code == STILL_ACTIVE as u32;
        CloseHandle(handle);
        alive
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn is_alive(_pid: u32) -> bool {
    true
}

/// 这个 pid 当前进程的启动时间（墙钟毫秒；Node `probeProcessStartTimeDefault`，修复线 F2）。取不到时为 None，
/// 调用方按 Node 保守处理（不回收）。Linux 读 `/proc/<pid>/stat` 的第 22 个字段（开机后的时钟滴答，按 Node 取每秒
/// 100 个）加开机时刻；macOS 用 `sysctl(KERN_PROC_PID)`（与 Node 用的 `ps -o lstart=` 同一来源，Node 取到秒）；Windows 用
/// `GetProcessTimes`。
#[cfg(target_os = "linux")]
#[expect(
    clippy::disallowed_methods,
    reason = "进程启动时间 = 现在 - 开机时长 + 启动滴答，与 Node 的锁回收同一算法；fs 在 kernel-api 之下，拿不到注入的 Clock"
)]
pub(crate) async fn start_time_ms(pid: u32) -> Option<u64> {
    use rust_cli_base::proto::procfs;
    use rust_cli_base::units::MS_PER_SECOND_F64;

    let stat = tokio::fs::read_to_string(procfs::stat_path(pid))
        .await
        .ok()?;
    let ticks: f64 = procfs::field(&stat, procfs::STARTTIME)?.parse().ok()?;
    let uptime: f64 = tokio::fs::read_to_string(procfs::UPTIME_PATH)
        .await
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs_f64()
        * MS_PER_SECOND_F64;
    let started =
        now - uptime * MS_PER_SECOND_F64 + ticks * (MS_PER_SECOND_F64 / procfs::USER_HZ as f64);
    (started.is_finite() && started > 0.0).then(|| started.round() as u64)
}

#[cfg(target_os = "macos")]
#[expect(unsafe_code, reason = "sysctl 没有安全封装")]
pub(crate) async fn start_time_ms(pid: u32) -> Option<u64> {
    /// `kp_proc.p_starttime`（`timeval`）在 `kinfo_proc` 开头占的字节数：8 字节秒、4 字节微秒与对齐。
    const TIMEVAL_BYTES: usize = 16;
    // `sysctl(CTL_KERN, KERN_PROC, KERN_PROC_PID, pid)` 的 `kinfo_proc`（`ps` 同一来源，别的用户的进程也读得到；
    // `proc_pidinfo` 读不了）。`kp_proc.p_starttime` 是结构开头的 `timeval`：8 字节秒、4 字节微秒。libc 没有
    // 这个结构，按字节读开头 16 字节；缓冲区比 `kinfo_proc`（64 位 648 字节）大，按 8 字节对齐。
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID,
        libc::c_int::try_from(pid).ok()?,
    ];
    let mut buffer = [0u64; 128];
    let mut size = std::mem::size_of_val(&buffer);
    // SAFETY: mib 与缓冲区都在栈上，长度如实传入；内核最多写 `size` 字节并回写实际长度。
    let status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buffer.as_mut_ptr().cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    // 进程不存在时成功但长度为 0。
    if status != 0 || size < TIMEVAL_BYTES {
        return None;
    }
    let seconds = i64::from_ne_bytes(buffer[0].to_ne_bytes());
    let micros = u32::from_ne_bytes(buffer[1].to_ne_bytes()[..4].try_into().ok()?);
    let seconds = u64::try_from(seconds).ok().filter(|seconds| *seconds > 0)?;
    Some(
        seconds * rust_cli_base::units::MS_PER_SECOND
            + u64::from(micros) / rust_cli_base::units::MICROS_PER_MS,
    )
}

#[cfg(windows)]
#[expect(unsafe_code, reason = "OpenProcess / GetProcessTimes 没有安全封装")]
pub(crate) async fn start_time_ms(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: 只在本函数内打开、查询并关闭同一个进程句柄，指针参数指向栈上变量。
    let queried = unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let ok = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) != 0;
        CloseHandle(handle);
        ok
    };
    // FILETIME 是 1601-01-01 起的 100 ns 数；与 Unix 纪元差 11644473600 秒。
    let ticks = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    queried
        .then(|| {
            (ticks / rust_cli_base::units::FILETIME_TICKS_PER_MS)
                .checked_sub(rust_cli_base::units::FILETIME_EPOCH_OFFSET_MS)
        })
        .flatten()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(crate) async fn start_time_ms(_pid: u32) -> Option<u64> {
    None
}
