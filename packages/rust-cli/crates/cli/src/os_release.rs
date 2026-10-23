//! 运行的平台与系统版本（修复线 F65）：TUI 按它判断按键的含义与颜色级别（docs/specs/rust-cli/tui-input.md §9、
//! tui.md §7.4）。入口读一次，经 `Options::os` 与信任、更新的选择屏传给 TUI。

use rust_cli_tui::{Os, WindowsRelease};

/// 本进程的平台。Windows 带系统版本（Node `os.release()` 的写法：libuv 用 `RtlGetVersion`，不受应用兼容清单影响）。
pub(crate) fn current() -> Os {
    if cfg!(windows) {
        Os::Windows(windows_release())
    } else {
        Os::Unix
    }
}

/// Node `os.release()`（工作线 F64：模型请求的 `x-os-version`，docs/specs/rust-cli/model.md §2.4）：Unix 是 `uname` 的
/// release（macOS 例如 `24.6.0`），Windows 是 `RtlGetVersion` 的 `<主>.<次>.<build>`（libuv 的写法，例如 `10.0.22631`）。
/// 读不到时为 None（不发这个头）。
pub(crate) fn node_release() -> Option<String> {
    #[cfg(windows)]
    {
        rtl_version().map(|info| {
            format!(
                "{}.{}.{}",
                info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber
            )
        })
    }
    #[cfg(unix)]
    {
        uname_release()
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

#[cfg(windows)]
#[expect(unsafe_code, reason = "RtlGetVersion 是 ntdll 的 FFI，没有安全封装")]
fn rtl_version() -> Option<windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW> {
    use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
    use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;

    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: u32::try_from(size_of::<OSVERSIONINFOW>()).ok()?,
        ..OSVERSIONINFOW::default()
    };
    // SAFETY: 只写 `info`，指针指向本函数里有效的内存；`dwOSVersionInfoSize` 是这个结构的大小。
    let status = unsafe { RtlGetVersion(&mut info) };
    (status == 0).then_some(info)
}

#[cfg(windows)]
fn windows_release() -> Option<WindowsRelease> {
    rtl_version().map(|info| WindowsRelease {
        major: info.dwMajorVersion,
        build: info.dwBuildNumber,
    })
}

#[cfg(not(windows))]
fn windows_release() -> Option<WindowsRelease> {
    None
}

#[cfg(unix)]
#[expect(unsafe_code, reason = "uname(2) 是 libc 的 FFI，没有安全封装")]
fn uname_release() -> Option<String> {
    let mut name = std::mem::MaybeUninit::<libc::utsname>::uninit();
    // SAFETY: uname 只写传入的结构体，指针指向本函数里有效的内存。
    if unsafe { libc::uname(name.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: 返回 0 表示结构体已写满；`release` 是以 NUL 结尾的 C 字符串。
    let name = unsafe { name.assume_init() };
    // SAFETY: 同上，`release` 在结构体里，以 NUL 结尾。
    let release = unsafe { std::ffi::CStr::from_ptr(name.release.as_ptr()) };
    Some(release.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    /// 本机的 `os.release()`：非空（工作线 F64）。
    #[test]
    fn the_node_release_is_reported() {
        assert!(super::node_release().is_some_and(|release| !release.is_empty()));
    }

    /// 本机的平台（在 Windows 上跑时读到真实的版本，主版本至少 10）。
    #[test]
    fn the_current_platform_is_reported() {
        let os = super::current();
        assert_eq!(os.is_windows(), cfg!(windows));
        if let rust_cli_tui::Os::Windows(release) = os {
            assert!(release.is_some_and(|r| r.major >= 10 && r.build > 0));
        }
    }
}
