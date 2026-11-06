//! 真实的 [`Desktop`] 端口：文件系统、子进程与 Windows 注册表（docs/specs/rust-cli/cli-args.md §16）。

use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;

use super::{Desktop, Hive};

pub(crate) struct System;

impl Desktop for System {
    async fn is_folder(&self, path: &Path) -> bool {
        rust_cli_fs::is_dir(path).await
    }

    async fn is_file(&self, path: &Path) -> bool {
        rust_cli_fs::is_file(path).await
    }

    async fn run(&self, program: &str, args: &[OsString]) -> Result<bool, String> {
        rust_cli_proc::command(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|status| status.success())
            .map_err(|error| error.to_string())
    }

    async fn start(&self, program: &Path, args: &[OsString]) -> Result<(), String> {
        // 丢掉返回的 Child 不会结束它；spawn 失败（权限、格式）在这里就报出来。
        rust_cli_proc::daemon::spawn(&mut launch_command(program, args))
            .map(drop)
            .map_err(|error| error.to_string())
    }

    async fn install_location(&self, hive: Hive, key: &str) -> Option<String> {
        #[cfg(windows)]
        {
            let key = key.to_owned();
            rust_cli_base::blocking(move || registry::install_location(hive, &key))
                .await
                .ok()
                .flatten()
        }
        #[cfg(not(windows))]
        {
            let _ = (hive, key);
            None
        }
    }
}

/// 启动桌面端的命令：stdio 全部接空，Windows 上脱离控制台与进程组（同 Node `spawn({detached: true})`；Unix 的
/// 自成会话由 `daemon::spawn` 做）。
fn launch_command(program: &Path, args: &[OsString]) -> tokio::process::Command {
    let mut command = rust_cli_proc::command(program);
    command
        .args(args)
        // 终端里带着 ELECTRON_RUN_AS_NODE 时 Electron 把自己当 Node 跑，会把 --open-workspace 当脚本。
        .env_remove("ELECTRON_RUN_AS_NODE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    command
}

#[cfg(windows)]
mod registry {
    use std::ptr::null_mut;

    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW,
    };

    use super::Hive;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }

    /// `<hive>\Software\<key>` 的 `InstallLocation`（REG_SZ）。直接读 UTF-16 的值：`reg.exe` 按控制台代码页输出，
    /// 非 ASCII 的安装路径会读坏（cli-args.md §16）。
    #[expect(unsafe_code, reason = "RegGetValueW 是 advapi32 的 FFI，没有安全封装")]
    pub(super) fn install_location(hive: Hive, key: &str) -> Option<String> {
        let root = match hive {
            Hive::CurrentUser => HKEY_CURRENT_USER,
            Hive::LocalMachine => HKEY_LOCAL_MACHINE,
        };
        let subkey = wide(&format!("Software\\{key}"));
        let value = wide("InstallLocation");
        let mut bytes: u32 = 0;
        // SAFETY: 两个名字是以 0 结尾的 UTF-16，在调用期间有效；数据指针为空，只写 `bytes`（值的字节数）。
        let status = unsafe {
            RegGetValueW(
                root,
                subkey.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                null_mut(),
                null_mut(),
                &mut bytes,
            )
        };
        if status != ERROR_SUCCESS || bytes == 0 {
            return None;
        }
        let mut buffer = vec![0u16; usize::try_from(bytes).ok()?.div_ceil(2)];
        let mut bytes = u32::try_from(buffer.len() * 2).ok()?;
        // SAFETY: `buffer` 有 `bytes` 个字节可写；RRF_RT_REG_SZ 只接受字符串值，结果以 0 结尾。两次调用之间值变长时
        // 返回 ERROR_MORE_DATA，按没有处理。
        let status = unsafe {
            RegGetValueW(
                root,
                subkey.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let end = buffer
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(buffer.len());
        String::from_utf16(&buffer[..end]).ok()
    }
}

#[cfg(all(test, unix))]
#[path = "system_tests.rs"]
mod tests;
