//! `zcode app [folder]`（docs/specs/rust-cli/cli-args.md §16）：让 ZCode 桌面端打开一个文件夹。
//!
//! 找到桌面端，带 `--open-workspace <文件夹>` 启动一个新进程，不等它。桌面端已在运行时新进程撞上单实例锁退出，
//! 运行中的那个经 second-instance 收到 argv、打开这个文件夹。不进终端界面，不建内核，不读配置。
//! 这里是纯逻辑，碰系统的部分经 [`Desktop`] 端口（真实的是 [`system::System`]），测试用假的。

mod system;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::Result;
use rust_cli_base::secret::EnvSnapshot;

pub(crate) use system::System;

const HELP: &str = "Usage: zcode app [options] [folder]

Open a folder in the ZCode desktop app (default: the current folder). With
ZCODE_ENV=test it opens ZCode Preview.

Options:
  -h, --help  Display help for command
";

/// 桌面端自己认的参数：本机的、可信的请求，不弹确认（`zcode://workspace/open` 要确认）。
const OPEN_ARG: &str = "--open-workspace";

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Help,
    /// 要打开的文件夹；None 是 cwd。
    Open(Option<String>),
}

/// 第一个参数是 `app` 时解析成命令（commander 的规则：先看帮助，再报未知选项，再报多余的参数）；错误是写到
/// stderr 的完整一行。其他命令返回 None。
pub(crate) fn parse(args: &[String]) -> Option<Result<Command, String>> {
    let (first, rest) = args.split_first()?;
    if first != "app" {
        return None;
    }
    let (options, operands) = match rest.iter().position(|arg| arg == "--") {
        Some(at) => (&rest[..at], &rest[at + 1..]),
        None => (rest, &[][..]),
    };
    if options.iter().any(|arg| arg == "-h" || arg == "--help") {
        return Some(Ok(Command::Help));
    }
    if let Some(option) = options
        .iter()
        .find(|arg| arg.starts_with('-') && *arg != "-")
    {
        return Some(Err(format!("error: unknown option '{option}'")));
    }
    let folders: Vec<&String> = options.iter().chain(operands).collect();
    Some(match folders.as_slice() {
        [] => Ok(Command::Open(None)),
        [folder] => Ok(Command::Open(Some((*folder).clone()))),
        more => Err(format!(
            "error: too many arguments for 'app'. Expected 1 argument but got {}.",
            more.len()
        )),
    })
}

/// 执行 `zcode app`：`env` 是入口的进程环境快照。失败是 `zcode: <原因>` 的原因（入口加前缀，退出码 1）。
#[expect(
    clippy::disallowed_methods,
    reason = "入口 crate 是唯一读取 cwd 的位置（spec §4）；文件夹相对 cwd 解析"
)]
pub(crate) async fn command(
    command: Command,
    env: &EnvSnapshot,
    out: &mut dyn std::io::Write,
) -> Result<u8> {
    match command {
        Command::Help => {
            let _ = out.write_all(HELP.as_bytes());
            Ok(0)
        }
        Command::Open(folder) => {
            let cwd = std::env::current_dir()?;
            // 与 origins 的约定相同：值去掉首尾空白，空值按没设（`EnvSnapshot::text`）。
            let var = |name: &str| env.text(name).map(str::to_owned);
            open(folder.as_deref(), &cwd, Platform::current(), &var, &System)
                .await
                .map_err(anyhow::Error::msg)?;
            Ok(0)
        }
    }
}

/// 桌面端的一种身份，取自 `packages/desktop/scripts/desktop-product-identity.mjs`。`windows_guid` 是
/// electron-builder 按 `app_id` 算的 UUID v5：NSIS 安装器把安装位置写在 `Software\<guid>` 下。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Identity {
    pub(crate) app_id: &'static str,
    pub(crate) product_name: &'static str,
    pub(crate) linux_executable: &'static str,
    pub(crate) windows_guid: &'static str,
}

pub(crate) const PRODUCTION: Identity = Identity {
    app_id: "dev.zcode.app",
    product_name: "ZCode",
    linux_executable: "zcode",
    windows_guid: "268ce9e6-a30b-5890-ad18-d4b3ebba5377",
};

pub(crate) const PREVIEW: Identity = Identity {
    app_id: "dev.zcode.app.preview",
    product_name: "ZCode Preview",
    linux_executable: "zcode-preview",
    windows_guid: "ae9417d7-4889-5caf-9177-cd8e16f266f2",
};

/// 与账号服务同一个后端（`origins::is_test`）：测试环境是 Preview，其他是正式的。没装时不退到另一个。
pub(crate) fn identity(var: &impl Fn(&str) -> Option<String>) -> Identity {
    if crate::assemble::origins::is_test(var) {
        PREVIEW
    } else {
        PRODUCTION
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Platform {
    Mac,
    Windows,
    /// Linux 与其他 Unix。
    Linux,
}

impl Platform {
    fn current() -> Self {
        match rust_cli_base::js::node_platform() {
            "darwin" => Self::Mac,
            "win32" => Self::Windows,
            _ => Self::Linux,
        }
    }
}

/// Windows 注册表的根。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Hive {
    CurrentUser,
    LocalMachine,
}

/// `zcode app` 碰到的进程外的东西。
pub(crate) trait Desktop {
    async fn is_folder(&self, path: &Path) -> bool;
    async fn is_file(&self, path: &Path) -> bool;
    /// 跑完 `program`：是否以 0 退出；起不来时是原因。
    async fn run(&self, program: &str, args: &[OsString]) -> Result<bool, String>;
    /// 脱离本进程启动 `program`，不等它；起不来时是原因。
    async fn start(&self, program: &Path, args: &[OsString]) -> Result<(), String>;
    /// `<hive>\Software\<key>` 的 `InstallLocation`；没有或不是字符串时 None。只在 Windows 上有值。
    async fn install_location(&self, hive: Hive, key: &str) -> Option<String>;
}

/// 让桌面端打开 `folder`（None：`cwd`）。错误是给用户看的原因。
pub(crate) async fn open(
    folder: Option<&str>,
    cwd: &Path,
    platform: Platform,
    var: &impl Fn(&str) -> Option<String>,
    desktop: &impl Desktop,
) -> Result<(), String> {
    let path = rust_cli_base::path::normalize(&cwd.join(folder.unwrap_or(".")));
    if !desktop.is_folder(&path).await {
        let shown = folder.map_or_else(|| path.display().to_string(), str::to_owned);
        return Err(format!("no folder “{shown}”"));
    }
    let id = identity(var);
    let missing =
        |looked: String| format!("can't find the {} desktop app ({looked})", id.product_name);
    let args = [OsString::from(OPEN_ARG), path.into_os_string()];
    match platform {
        Platform::Mac => {
            // -n：不带它时 open 对已在运行的应用只激活、丢掉参数；带它时新进程撞上单实例锁，把 argv 交给运行中的那个。
            let mut open_args: Vec<OsString> = ["-n", "-b", id.app_id, "--args"]
                .into_iter()
                .map(OsString::from)
                .collect();
            open_args.extend(args);
            match desktop.run("open", &open_args).await {
                Ok(true) => Ok(()),
                Ok(false) => Err(missing(format!("no app with id {}", id.app_id))),
                Err(why) => Err(format!("couldn't run open: {why}")),
            }
        }
        Platform::Windows => match windows_executable(id, var, desktop).await {
            Some(exe) => launch(&exe, &args, desktop).await,
            None => Err(missing(format!(
                "not in the registry or %LOCALAPPDATA%\\Programs\\{}",
                id.product_name
            ))),
        },
        Platform::Linux => {
            // 不在 PATH 里找：桌面端在 Linux 上的可执行名也是 zcode，会找到本工程自己。
            let exe = Path::new("/opt")
                .join(id.product_name)
                .join(id.linux_executable);
            if desktop.is_file(&exe).await {
                launch(&exe, &args, desktop).await
            } else {
                Err(missing(format!("no {}", exe.display())))
            }
        }
    }
}

async fn launch(exe: &Path, args: &[OsString], desktop: &impl Desktop) -> Result<(), String> {
    desktop
        .start(exe, args)
        .await
        .map_err(|why| format!("couldn't start {}: {why}", exe.display()))
}

/// Windows 安装器放可执行文件的地方：只为自己安装、为所有用户安装，再是默认位置；第一个存在的。
async fn windows_executable(
    id: Identity,
    var: &impl Fn(&str) -> Option<String>,
    desktop: &impl Desktop,
) -> Option<PathBuf> {
    let exe = format!("{}.exe", id.product_name);
    for hive in [Hive::CurrentUser, Hive::LocalMachine] {
        let Some(dir) = desktop
            .install_location(hive, id.windows_guid)
            .await
            .filter(|dir| !dir.is_empty())
        else {
            continue;
        };
        let candidate = PathBuf::from(windows_join(&dir, &exe));
        if desktop.is_file(&candidate).await {
            return Some(candidate);
        }
    }
    let local = var("LOCALAPPDATA").filter(|dir| !dir.is_empty())?;
    let candidate = PathBuf::from(
        ["Programs", id.product_name, &exe]
            .into_iter()
            .fold(local, |dir, name| windows_join(&dir, name)),
    );
    desktop.is_file(&candidate).await.then_some(candidate)
}

/// Windows 路径按字符串拼（测试在任何平台上跑）。
fn windows_join(dir: &str, name: &str) -> String {
    format!("{}\\{name}", dir.trim_end_matches(['\\', '/']))
}

#[cfg(test)]
#[path = "desktop_app_tests.rs"]
mod tests;
