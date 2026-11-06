//! 界面的剪贴板端口（`rust_cli_tui::services::Clipboard`；tui-input.md §3.1、tui-select.md §3.2）：读剪贴板上的图片
//! （`clipboard/image.rs`）、读图片文件与写文字都按平台选命令；子进程只经 proc，读写文件只经 fs。写文字的命令从标准输入读。
use std::path::PathBuf;
use std::process::Stdio;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rust_cli_tui::services::{Clipboard, CopyRoute, ImageBytes};
use tokio::io::AsyncWriteExt;

mod image;

/// PowerShell 写剪贴板：文字从标准输入读，按 UTF-8。
const SET_CLIPBOARD: &str = "[Console]::InputEncoding = [Text.Encoding]::UTF8; Set-Clipboard -Value ([Console]::In.ReadToEnd())";

/// 每个写剪贴板的命令最多等这么久（2000 ms）。
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Clone, Default)]
pub(crate) struct SystemClipboard {
    /// `WAYLAND_DISPLAY` 有值：Linux 先试 `wl-paste` / `wl-copy`。
    pub wayland: bool,
    /// `DISPLAY` 有值：Linux 写剪贴板时试 xclip、xsel（先探测有没有这个命令）。
    pub display: bool,
    /// `SSH_CONNECTION` 有值：剪贴板在远端，不起本机的剪贴板命令。
    pub ssh: bool,
    /// `TMUX` 有值：另写 tmux 的缓冲区。
    pub tmux: bool,
    /// WSL（`WSL_DISTRO_NAME` 有值）：写剪贴板用 `powershell.exe`。
    pub wsl: bool,
}

/// 本机的写剪贴板命令：按顺序试，第一个起得来的就是。每一项是一组一起跑的
/// 命令（xclip、xsel 同时写 clipboard 与 primary；wl-copy 先写 clipboard 再写 primary）。
pub(crate) fn writers(clipboard: &SystemClipboard) -> Vec<Vec<(&'static str, Vec<&'static str>)>> {
    if cfg!(target_os = "macos") {
        return vec![vec![("pbcopy", Vec::new())]];
    }
    let powershell = |program| {
        vec![vec![(
            program,
            vec!["-NoProfile", "-NonInteractive", "-Command", SET_CLIPBOARD],
        )]]
    };
    if cfg!(windows) {
        return powershell("powershell");
    }
    if clipboard.wsl {
        return powershell("powershell.exe");
    }
    let mut out = Vec::new();
    if clipboard.wayland {
        out.push(vec![
            ("wl-copy", Vec::new()),
            ("wl-copy", vec!["--primary"]),
        ]);
    }
    if clipboard.display {
        out.push(vec![
            ("xclip", vec!["-selection", "clipboard"]),
            ("xclip", vec!["-selection", "primary"]),
        ]);
        out.push(vec![
            ("xsel", vec!["--clipboard", "--input"]),
            ("xsel", vec!["--primary", "--input"]),
        ]);
    }
    out
}

/// 起一个命令，把 `text` 写进它的标准输入，等它结束（最多 2 s）。命令起不来（没装）时为 None，否则是它是否成功。
async fn feed(program: &str, args: &[&str], text: &str) -> Option<bool> {
    let mut child = rust_cli_proc::command(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let write = async {
        if let Some(mut stdin) = child.stdin.take() {
            // 写失败（命令提前退出）只影响这一次复制。
            let _ = stdin.write_all(text.as_bytes()).await;
            let _ = stdin.shutdown().await;
        }
        child.wait().await.is_ok_and(|status| status.success())
    };
    Some(
        tokio::time::timeout(WRITE_TIMEOUT, write)
            .await
            .unwrap_or(false),
    )
}

/// 写系统剪贴板；返回有没有找到能用的命令（Linux 上决定提示的途径）。
async fn write_native(clipboard: &SystemClipboard, text: &str) -> bool {
    for group in writers(clipboard) {
        let mut found = false;
        for (program, args) in &group {
            match feed(program, args, text).await {
                Some(ok) => {
                    found = true;
                    if !ok {
                        tracing::debug!(program, "clipboard: write failed");
                    }
                }
                None => break,
            }
        }
        if found {
            return true;
        }
    }
    false
}

/// tmux 的缓冲区：`tmux load-buffer -w -`，失败时再试不带 `-w`（老版本的 tmux 没有 `-w`）。
async fn write_tmux(text: &str) -> bool {
    if feed("tmux", &["load-buffer", "-w", "-"], text).await == Some(true) {
        return true;
    }
    feed("tmux", &["load-buffer", "-"], text).await == Some(true)
}

impl Clipboard for SystemClipboard {
    fn copy_text(&self, text: String) -> BoxFuture<'static, CopyRoute> {
        let clipboard = self.clone();
        async move {
            // SSH 里剪贴板在远端：只有 OSC 52（TUI 写）与 tmux 缓冲区。
            let native = !clipboard.ssh && write_native(&clipboard, &text).await;
            if clipboard.tmux {
                write_tmux(&text).await;
            }
            route(&clipboard, native)
        }
        .boxed()
    }

    fn image(&self) -> BoxFuture<'static, Result<Option<ImageBytes>, String>> {
        image::read_image(self.wayland).boxed()
    }

    fn image_file(&self, path: String) -> BoxFuture<'static, Result<Option<ImageBytes>, String>> {
        async move {
            let bytes = rust_cli_fs::read_optional(&PathBuf::from(path))
                .await
                .map_err(|error| error.to_string())?;
            Ok(bytes.map(|bytes| ImageBytes { bytes }))
        }
        .boxed()
    }
}

/// 提示用的途径：不在 SSH 里时 macOS、Windows、WSL 是 native，Linux 找到了命令才是；否则在 tmux 里是
/// tmux 缓冲区；否则只有 OSC 52。
pub(crate) fn route(clipboard: &SystemClipboard, native_found: bool) -> CopyRoute {
    let native_platform = cfg!(target_os = "macos") || cfg!(windows) || clipboard.wsl;
    if !clipboard.ssh && (native_platform || native_found) {
        return CopyRoute::Native;
    }
    if clipboard.tmux {
        return CopyRoute::TmuxBuffer;
    }
    CopyRoute::Osc52
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_platform_has_its_clipboard_writer() {
        let all = SystemClipboard {
            wayland: true,
            display: true,
            ..SystemClipboard::default()
        };
        let writers = writers(&all);
        if cfg!(target_os = "macos") {
            assert_eq!(writers, vec![vec![("pbcopy", Vec::new())]]);
        } else if cfg!(windows) {
            assert_eq!(writers[0][0].0, "powershell");
            assert!(writers[0][0].1.contains(&SET_CLIPBOARD));
        } else {
            let programs: Vec<&str> = writers.iter().map(|group| group[0].0).collect();
            assert_eq!(programs, ["wl-copy", "xclip", "xsel"]);
            assert_eq!(writers[1][1].1, ["-selection", "primary"]);
            // 没有图形界面的 Linux：没有命令，途径是 OSC 52。
            assert!(super::writers(&SystemClipboard::default()).is_empty());
        }
    }

    #[test]
    fn the_route_follows_ssh_and_tmux() {
        let ssh = SystemClipboard {
            ssh: true,
            ..SystemClipboard::default()
        };
        assert_eq!(route(&ssh, false), CopyRoute::Osc52);
        let ssh_tmux = SystemClipboard {
            ssh: true,
            tmux: true,
            ..SystemClipboard::default()
        };
        assert_eq!(route(&ssh_tmux, false), CopyRoute::TmuxBuffer);
        assert_eq!(route(&SystemClipboard::default(), true), CopyRoute::Native);
        let local = route(&SystemClipboard::default(), false);
        if cfg!(target_os = "macos") || cfg!(windows) {
            assert_eq!(local, CopyRoute::Native);
        } else {
            assert_eq!(local, CopyRoute::Osc52);
        }
    }
}
