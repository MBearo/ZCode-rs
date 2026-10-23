// 复制自 rust/crates/proc/src/job.rs（feat/rs 79b6691c55）。
//! Windows Job Object：整棵子进程树随 Job 一起结束。

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use tokio::process::Child;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_LIMIT_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};

/// [`Job::terminate`] 结束进程时使用的退出码（与 `taskkill /F` 相同）。
const TERMINATED_EXIT_CODE: u32 = 1;

/// 设置了 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 的匿名 Job Object。
///
/// 进程加入后，它此后创建的后代自动属于同一个 Job。[`Job::terminate`] 结束其中所有进程；
/// `Job` 被 drop 时句柄关闭，其中仍在运行的进程同样被结束——本进程崩溃时系统关闭句柄，效果相同。
///
/// 加入不是追溯的：spawn 与 [`Job::assign`] 之间子进程已经创建的后代不在 Job 中，所以要在 spawn
/// 之后立即加入。
#[derive(Debug)]
pub struct Job {
    handle: OwnedHandle,
}

impl Job {
    /// 创建一个空的 Job。
    #[expect(unsafe_code, reason = "Win32 Job Object API 没有安全封装")]
    pub fn new() -> io::Result<Self> {
        // SAFETY: 两个参数都允许为空：默认安全属性、匿名 Job。
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: raw 是刚创建的有效句柄，只交给这一个 OwnedHandle，由它在 drop 时关闭一次。
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
            BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                ..Default::default()
            },
            ..Default::default()
        };
        let size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
            .map_err(io::Error::other)?;
        // SAFETY: 句柄有效；指针指向与信息类别匹配的结构体，长度与之一致，调用期间一直有效。
        let configured = unsafe {
            SetInformationJobObject(
                handle.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size,
            )
        };
        if configured == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle })
    }

    /// 把 `child` 加入 Job。`child` 已被回收时返回错误。
    ///
    /// 嵌套 Job 不允许加入时（例如本进程所在的 Job 禁止嵌套）也返回错误，调用方据此退回其他方式。
    #[expect(unsafe_code, reason = "Win32 Job Object API 没有安全封装")]
    pub fn assign(&self, child: &Child) -> io::Result<()> {
        let process = child
            .raw_handle()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "child already reaped"))?;
        // SAFETY: Job 句柄由 self 持有；进程句柄由 child 持有且子进程尚未回收，调用期间都有效。
        let assigned = unsafe { AssignProcessToJobObject(self.handle.as_raw_handle(), process) };
        if assigned == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// 结束 Job 中的所有进程（退出码 1）。其中已没有进程时同样成功。
    #[expect(unsafe_code, reason = "Win32 Job Object API 没有安全封装")]
    pub fn terminate(&self) -> io::Result<()> {
        // SAFETY: Job 句柄由 self 持有，调用期间有效。
        let terminated =
            unsafe { TerminateJobObject(self.handle.as_raw_handle(), TERMINATED_EXIT_CODE) };
        if terminated == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// `child` 属于这个 Job（测试用）。
    #[cfg(test)]
    #[expect(unsafe_code, reason = "Win32 Job Object API 没有安全封装")]
    pub(crate) fn holds(&self, child: &Child) -> bool {
        let Some(process) = child.raw_handle() else {
            return false;
        };
        let mut result = 0;
        // SAFETY: 两个句柄在调用期间都有效；result 是本函数的局部变量。
        let queried = unsafe {
            windows_sys::Win32::System::JobObjects::IsProcessInJob(
                process,
                self.handle.as_raw_handle(),
                &raw mut result,
            )
        };
        queried != 0 && result != 0
    }
}

#[cfg(test)]
#[path = "job_tests.rs"]
mod tests;
