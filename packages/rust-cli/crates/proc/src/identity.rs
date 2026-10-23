//! 进程身份：PID 加启动时间，区分 PID 复用（docs/specs/rust-cli/tools-more.md §16.1）。
//!
//! 启动时间的写法：`LC_ALL=C TZ=UTC ps -o lstart= -p <pid>`
//! 的输出去掉首尾空白。Windows 不取（返回 None，调用方按“未知”处理）。

/// `pid` 的启动时间；进程不在、查不到或超时时为 None。
#[cfg(unix)]
pub async fn process_started(pid: u32) -> Option<String> {
    use std::process::Stdio;
    use std::time::Duration;
    // `ps` 的等待上限。
    const LOOKUP_TIMEOUT: Duration = Duration::from_millis(1_000);
    let lookup = async {
        let output = crate::command("ps")
            .args(["-o", "lstart=", "-p", &pid.to_string()])
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .ok()?;
        let text = String::from_utf8(output.stdout).ok()?;
        let started = text.trim();
        (output.status.success() && !started.is_empty()).then(|| started.to_owned())
    };
    tokio::time::timeout(LOOKUP_TIMEOUT, lookup)
        .await
        .ok()
        .flatten()
}

/// Windows 不取启动时间。
#[cfg(not(unix))]
pub async fn process_started(_pid: u32) -> Option<String> {
    None
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;
