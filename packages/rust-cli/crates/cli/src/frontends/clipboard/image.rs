//! 读剪贴板上的图片（tui-input.md §3.1，界面的 `Clipboard::image`）：直接读图片字节，按平台试命令。
use base64::Engine;
use rust_cli_base::proto::mime;
use rust_cli_tui::services::ImageBytes;
use std::process::Stdio;

/// PowerShell：剪贴板里有图片时打印 PNG 的 base64，没有时退出码 1（`Get-Clipboard -Format Image`）。
const WIN_IMAGE: &str = "$i = Get-Clipboard -Format Image; if ($i) { $m = New-Object System.IO.MemoryStream; $i.Save($m, [System.Drawing.Imaging.ImageFormat]::Png); [Convert]::ToBase64String($m.ToArray()) } else { exit 1 }";

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

/// `«data PNGf89504E47…»` → 字节；不是这个形状时为 None。
pub(crate) fn parse_osascript(output: &[u8]) -> Option<Vec<u8>> {
    let text = String::from_utf8_lossy(output);
    let hex = text.trim().strip_prefix("«data PNGf")?.strip_suffix('»')?;
    if hex.len() % 2 != 0 {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).ok())
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
            let out = run("powershell", &["-NoProfile", "-Command", WIN_IMAGE]).await?;
            let text = String::from_utf8_lossy(&out);
            base64::engine::general_purpose::STANDARD
                .decode(text.trim())
                .ok()
        }
    }
}

/// 依次试本平台的命令，第一个读到图片的就是；都没有时 `Ok(None)`。
pub(super) async fn read_image(wayland: bool) -> Result<Option<ImageBytes>, String> {
    for reader in &readers(wayland) {
        if let Some(bytes) = read(reader).await {
            return Ok(Some(ImageBytes { bytes }));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

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
