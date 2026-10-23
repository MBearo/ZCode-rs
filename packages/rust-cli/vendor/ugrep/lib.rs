//! ugrep 7.8.4 作为库（docs/specs/rust-cli/embedded-search.md §7）：zcode 以 `ugrep` 的名字启动时调用 [`main`]。
//! ugrep 的 `main()`（Windows 是 `wmain()`）编译时改名（build.rs 的 `-Dmain=…`），`shim.cpp` 给它一个 C 链接的入口；
//! 源码不改。

// 只在 C++ 里引用的库：让 rustc 把它们的静态库链接进来。
extern crate bzip2_sys as _;
extern crate libz_sys as _;
extern crate pcre2_sys as _;
extern crate zstd_sys as _;

use std::ffi::{OsString, c_int};

#[cfg(unix)]
unsafe extern "C" {
    fn rust_cli_ugrep_main(argc: c_int, argv: *const *const std::ffi::c_char) -> c_int;
}

#[cfg(windows)]
unsafe extern "C" {
    fn rust_cli_ugrep_wmain(argc: c_int, wargv: *const *const u16) -> c_int;
}

/// 跑 ugrep：`args` 是 argv[0] 之后的参数；argv[0] 换成 `ugrep`（ugrep 按 argv[0] 决定是不是 `ug`，`ug` 会读配置文件）。
/// 返回退出码。
#[cfg(unix)]
pub fn main(args: impl IntoIterator<Item = OsString>) -> i32 {
    use std::ffi::{CString, c_char};
    use std::os::unix::ffi::OsStringExt;
    let mut owned = vec![CString::from(c"ugrep")];
    for arg in args {
        // Unix 的参数是 C 字符串，不会含 NUL。
        match CString::new(arg.into_vec()) {
            Ok(arg) => owned.push(arg),
            Err(_) => return 2,
        }
    }
    let mut argv: Vec<*const c_char> = owned.iter().map(|arg| arg.as_ptr()).collect();
    argv.push(std::ptr::null());
    let Ok(argc) = c_int::try_from(owned.len()) else {
        return 2;
    };
    // SAFETY: Rust 运行时启动时把 SIGPIPE 设成忽略；先恢复缺省，ugrep 的 main 再按它自己的方式设（它忽略 SIGPIPE、
    // 自己处理 EPIPE）。这时进程里只有主线程。
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    // SAFETY: argv 是 argc 个以 NUL 结尾的字符串加一个空指针，`owned` 在调用期间一直有效；ugrep 不写也不保留它们。
    unsafe { rust_cli_ugrep_main(argc, argv.as_ptr()) }
}

/// Windows：参数按 UTF-16 原文交给 ugrep 的 `wmain`（它自己转成 UTF-8）；argv[0] 换成 `ugrep`。
#[cfg(windows)]
pub fn main(args: impl IntoIterator<Item = OsString>) -> i32 {
    use std::os::windows::ffi::OsStrExt;
    let wide = |text: &std::ffi::OsStr| -> Vec<u16> { text.encode_wide().chain([0]).collect() };
    let mut owned = vec![wide(std::ffi::OsStr::new("ugrep"))];
    owned.extend(args.into_iter().map(|arg| wide(&arg)));
    let mut wargv: Vec<*const u16> = owned.iter().map(|arg| arg.as_ptr()).collect();
    wargv.push(std::ptr::null());
    let Ok(argc) = c_int::try_from(owned.len()) else {
        return 2;
    };
    // SAFETY: wargv 是 argc 个以 0 结尾的 UTF-16 字符串加一个空指针，`owned` 在调用期间一直有效。
    unsafe { rust_cli_ugrep_wmain(argc, wargv.as_ptr()) }
}
