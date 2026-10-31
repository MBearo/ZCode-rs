//! TUI 的剪贴板（docs/specs/rust-cli/tui-input.md §3.1、tui-select.md §3.2）：读图片与写文字都按平台选命令；
//! 子进程只经 proc，读文件只经 fs。
//! 读图片的命令把图片写到标准输出，不落临时文件：macOS `osascript` 打印 `«data PNGf…»`（十六进制），Linux `wl-paste` /
//! `xclip` 打印字节，Windows PowerShell 打印 base64。写文字的命令从标准输入读。
use rust_cli_base::proto::mime;
use std::path::PathBuf;
use std::process::Stdio;

use base64::Engine;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rust_cli_tui::{Clipboard, CopyRoute, ImageBytes};
use tokio::io::AsyncWriteExt;

use crate::consts;

/// PowerShell：剪贴板里有图片时打印 PNG 的 base64，没有时退出码 1（`Get-Clipboard -Format Image`）。
const POWERSHELL: &str = "$i = Get-Clipboard -Format Image; if ($i) { $m = New-Object System.IO.MemoryStream; $i.Save($m, [System.Drawing.Imaging.ImageFormat]::Png); [Convert]::ToBase64String($m.ToArray()) } else { exit 1 }";

/// PowerShell 写剪贴板：文字从标准输入读，按 UTF-8。
const SET_CLIPBOARD: &str = "[Console]::InputEncoding = [Text.Encoding]::UTF8; Set-Clipboard -Value ([Console]::In.ReadToEnd())";

/// 每个写剪贴板的命令最多等这么久（2000 ms）。
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug, Clone, Default)]
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

/// 一个读剪贴板的命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reader {
    /// macOS：输出是 `«data PNGf<十六进制>»`。
    Osascript,
    /// Linux：输出是 PNG 字节。
    Bytes(&'static str, &'static [&'static str]),
    /// Windows：输出是 base64。
    PowerShell,
}

/// 本平台按顺序试的命令（macOS `osascript`；Linux `xclip`，Wayland 下另有 `wl-paste`；Windows PowerShell）。
pub(crate) fn readers(wayland: bool) -> Vec<Reader> {
    if cfg!(target_os = "macos") {
        return vec![Reader::Osascript];
    }
    if cfg!(windows) {
        return vec![Reader::PowerShell];
    }
    let mut out = Vec::new();
    if wayland {
        out.push(Reader::Bytes("wl-paste", &["--type", mime::PNG]));
    }
    out.push(Reader::Bytes(
        "xclip",
        &["-selection", "clipboard", "-t", mime::PNG, "-o"],
    ));
    out
}

/// 一个字节的十六进制位数。
const HEX_DIGITS_PER_BYTE: usize = 2;

/// `«data PNGf89504E47…»` → 字节；不是这个形状时为 None。
pub(crate) fn parse_osascript(output: &[u8]) -> Option<Vec<u8>> {
    let text = String::from_utf8_lossy(output);
    let hex = text.trim().strip_prefix("«data PNGf")?.strip_suffix('»')?;
    if hex.len() % HEX_DIGITS_PER_BYTE != 0 {
        return None;
    }
    (0..hex.len())
        .step_by(HEX_DIGITS_PER_BYTE)
        .map(|at| u8::from_str_radix(&hex[at..at + HEX_DIGITS_PER_BYTE], consts::HEX_RADIX).ok())
        .collect()
}

/// 跑一个命令，成功时给出标准输出（没装、失败都是 None）。
async fn run(program: &str, args: &[&str]) -> Option<Vec<u8>> {
    let output = rust_cli_proc::command(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    output.status.success().then_some(output.stdout)
}

async fn read(reader: &Reader) -> Option<Vec<u8>> {
    match reader {
        Reader::Osascript => {
            let out = run("osascript", &["-e", "the clipboard as «class PNGf»"]).await?;
            parse_osascript(&out)
        }
        Reader::Bytes(program, args) => {
            let out = run(program, args).await?;
            // 文件头判断（kernel-api `image::media_type`，与 Read、TUI 的粘贴同一个判断，F58）。
            (rust_cli_kernel_api::image::media_type(&out) == Some(mime::PNG)).then_some(out)
        }
        Reader::PowerShell => {
            let out = run("powershell", &["-NoProfile", "-Command", POWERSHELL]).await?;
            let text = String::from_utf8_lossy(&out);
            base64::engine::general_purpose::STANDARD
                .decode(text.trim())
                .ok()
        }
    }
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
        let readers = readers(self.wayland);
        async move {
            for reader in &readers {
                if let Some(bytes) = read(reader).await {
                    return Ok(Some(ImageBytes { bytes }));
                }
            }
            Ok(None)
        }
        .boxed()
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

    #[test]
    fn osascript_hex_output_decodes_to_bytes() {
        assert_eq!(
            parse_osascript("«data PNGf89504E47»\n".as_bytes()),
            Some(vec![0x89, 0x50, 0x4e, 0x47])
        );
        assert_eq!(parse_osascript(b"text"), None);
        assert_eq!(parse_osascript("«data PNGf8»".as_bytes()), None);
    }

    #[test]
    fn each_platform_has_its_clipboard_reader() {
        let readers = readers(true);
        if cfg!(target_os = "macos") {
            assert_eq!(readers, [Reader::Osascript]);
        } else if cfg!(windows) {
            assert_eq!(readers, [Reader::PowerShell]);
        } else {
            assert_eq!(readers.len(), 2);
            assert!(matches!(readers[0], Reader::Bytes("wl-paste", _)));
        }
    }
}
