//! bfs 4.1.4（带 Oniguruma 6.9.10）作为库（docs/specs/rust-cli/embedded-search.md §6）：zcode 以 `bfs` 的名字启动时
//! 调用 [`main`]。bfs 的 `main()` 编译时改名为 `rust_cli_bfs_main`（build.rs 的 `-Dmain=…`），源码不改。只在 Unix 上有。

#[cfg(unix)]
mod unix {
    use std::ffi::{CString, OsString, c_char, c_int};
    use std::os::unix::ffi::OsStringExt;

    unsafe extern "C" {
        fn rust_cli_bfs_main(argc: c_int, argv: *mut *mut c_char) -> c_int;
    }

    /// 跑 bfs：`args` 是 argv[0] 之后的参数；argv[0] 换成 `bfs`（报错的前缀与 `exec -a bfs` 相同）。返回退出码。
    pub fn main(args: impl IntoIterator<Item = OsString>) -> i32 {
        let mut owned = vec![CString::from(c"bfs")];
        for arg in args {
            // Unix 的参数是 C 字符串，不会含 NUL；万一有，按 bfs 自己的参数错误退出。
            match CString::new(arg.into_vec()) {
                Ok(arg) => owned.push(arg),
                Err(_) => return 1,
            }
        }
        let mut argv: Vec<*mut c_char> = owned.iter().map(|arg| arg.as_ptr().cast_mut()).collect();
        argv.push(std::ptr::null_mut());
        let Ok(argc) = c_int::try_from(owned.len()) else {
            return 1;
        };
        // SAFETY: Rust 运行时启动时把 SIGPIPE 设成忽略；bfs 是 C 程序，按缺省处理断开的管道（与系统的 find 相同），
        // 它 `-exec` 起的子进程也继承缺省的处理。这时进程里只有主线程。
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
        // SAFETY: argv 是 argc 个以 NUL 结尾的字符串加一个空指针，`owned` 在调用期间一直有效；bfs 只改指针数组里的
        // 项（模糊匹配的提示），不写字符串本身，不在返回后保留指针。
        unsafe { rust_cli_bfs_main(argc, argv.as_mut_ptr()) }
    }
}

#[cfg(unix)]
pub use unix::main;
