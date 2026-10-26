//! npm 平台包的纯计算部分（docs/specs/rust-cli/remote-exec.md §26.3、§26.4）：包名、`dist.integrity` 的校验、从 tarball
//! 解出执行器并核对 `build.json` 与 ELF 头。不做 IO。

use std::io::Read;

use base64::Engine;
use rust_cli_ssh_connect::Arch;
use serde_json::Value;
use sha2::{Digest, Sha256, Sha512};

/// 平台包名的前缀（ZCode-rs 的发布脚本 `scripts/rust-cli/npm/package.mjs`：`@mbearo/zcode-<id>`）。
const PACKAGE_PREFIX: &str = "@mbearo/zcode-";
/// 包里的执行器与构建记录（发布脚本放的位置；npm 的 tarball 里每项都在 `package/` 下）。
const BINARY_ENTRY: &str = "package/bin/zcode-rs";
const BUILD_ENTRY: &str = "package/build.json";
/// 包里一项最多读这么多（执行器几十 MB；防止坏的 tar 头让内存失控）。
const MAX_ENTRY: u64 = 512 * 1024 * 1024;
const BLOCK: usize = 512;

/// 远端架构对应的平台包（§26.3：只有 glibc 的包）。
pub(super) fn package_name(arch: Arch) -> String {
    let id = match arch {
        Arch::X86_64 => "linux-x64-gnu",
        Arch::Aarch64 => "linux-arm64-gnu",
    };
    format!("{PACKAGE_PREFIX}{id}")
}

/// `dist.integrity`（SRI，空白分隔的几项）里有一项 `sha512-<base64>` 与 `bytes` 的 sha512 相同。没有 sha512 的一项算不对。
pub(super) fn integrity_matches(bytes: &[u8], integrity: &str) -> bool {
    let digest = Sha512::digest(bytes);
    integrity
        .split_whitespace()
        .filter_map(|item| item.strip_prefix("sha512-"))
        .map(|encoded| encoded.split('?').next().unwrap_or(encoded))
        .filter_map(|encoded| {
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .ok()
        })
        .any(|expected| expected.as_slice() == digest.as_slice())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 从 tarball（gzip + tar）解出执行器，核对 `build.json`（版本、target、sha256）与 ELF 头。错误是一句简短的原因。
pub(super) fn extract(tarball: &[u8], version: &str, arch: Arch) -> Result<Vec<u8>, String> {
    let found = entries(flate2::read::GzDecoder::new(tarball))
        .map_err(|error| format!("cannot read the package: {error}"))?;
    let binary = found.binary.ok_or("the package has no bin/zcode-rs")?;
    let build = found.build.ok_or("the package has no build.json")?;
    let build: Value = serde_json::from_slice(&build)
        .map_err(|error| format!("build.json is not JSON: {error}"))?;
    let field = |name: &str| build.get(name).and_then(Value::as_str).unwrap_or("");
    if field("version") != version {
        return Err(format!(
            "build.json's version is {:?}, expected {version}",
            field("version")
        ));
    }
    let target = arch.gnu_triple();
    if field("target") != target {
        return Err(format!(
            "build.json's target is {:?}, expected {target}",
            field("target")
        ));
    }
    if !field("sha256").eq_ignore_ascii_case(&hex(&Sha256::digest(&binary))) {
        return Err("build.json's sha256 does not match bin/zcode-rs".to_owned());
    }
    if Arch::from_elf(&binary) != Some(arch) {
        return Err(format!("bin/zcode-rs is not a Linux {target} executable"));
    }
    Ok(binary)
}

/// 包里要的两项。
#[derive(Default)]
struct Entries {
    binary: Option<Vec<u8>>,
    build: Option<Vec<u8>>,
}

/// 读 tar：取执行器与 `build.json` 两项（普通文件；pax 头里的 `path` 认，别的头跳过）。
fn entries(mut reader: impl Read) -> std::io::Result<Entries> {
    let mut found = Entries::default();
    let mut pax_path: Option<String> = None;
    let mut header = [0u8; BLOCK];
    loop {
        if !read_block(&mut reader, &mut header)? || header.iter().all(|byte| *byte == 0) {
            break;
        }
        let size = size(&header)?;
        let padding = (BLOCK as u64 - size % BLOCK as u64) % BLOCK as u64;
        let name = pax_path.take().unwrap_or_else(|| name(&header));
        let wanted =
            matches!(header[156], b'0' | 0) && (name == BINARY_ENTRY || name == BUILD_ENTRY);
        if header[156] == b'x' || wanted {
            if size > MAX_ENTRY {
                return Err(invalid(format!("{name} is too large")));
            }
            let mut data = Vec::new();
            (&mut reader).take(size).read_to_end(&mut data)?;
            if data.len() as u64 != size {
                return Err(invalid(format!("{name} is cut short")));
            }
            skip(&mut reader, padding)?;
            match header[156] {
                b'x' => pax_path = pax_value(&data, "path"),
                _ if name == BINARY_ENTRY => found.binary = Some(data),
                _ => found.build = Some(data),
            }
        } else {
            skip(&mut reader, size + padding)?;
        }
    }
    Ok(found)
}

fn invalid(message: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

/// 读一个 512 字节的块；在块的开头读到结束时返回 false。
fn read_block(reader: &mut impl Read, block: &mut [u8; BLOCK]) -> std::io::Result<bool> {
    let mut filled = 0;
    while filled < BLOCK {
        match reader.read(&mut block[filled..])? {
            0 if filled == 0 => return Ok(false),
            0 => return Err(invalid("a tar header is cut short".to_owned())),
            read => filled += read,
        }
    }
    Ok(true)
}

fn skip(reader: &mut impl Read, bytes: u64) -> std::io::Result<()> {
    let skipped = std::io::copy(&mut reader.take(bytes), &mut std::io::sink())?;
    if skipped == bytes {
        Ok(())
    } else {
        Err(invalid("the archive is cut short".to_owned()))
    }
}

/// 头里的大小：八进制文本，或（最高位置 1 时）大端的二进制。
fn size(header: &[u8; BLOCK]) -> std::io::Result<u64> {
    let field = &header[124..136];
    if field[0] & 0x80 != 0 {
        return Ok(field[1..]
            .iter()
            .fold(0u64, |value, byte| (value << 8) | u64::from(*byte)));
    }
    let text = String::from_utf8_lossy(field);
    let text = text.trim_matches(|c: char| c == '\0' || c == ' ');
    if text.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(text, 8).map_err(|_| invalid(format!("bad tar size {text:?}")))
}

/// 头里的名字：ustar 的 `prefix/name`。
fn name(header: &[u8; BLOCK]) -> String {
    let text = |bytes: &[u8]| {
        let end = bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..end]).into_owned()
    };
    let name = text(&header[..100]);
    let prefix = if &header[257..262] == b"ustar" {
        text(&header[345..500])
    } else {
        String::new()
    };
    if prefix.is_empty() {
        name
    } else {
        format!("{prefix}/{name}")
    }
}

/// pax 头的一项（`<长度> <键>=<值>\n` 的记录）。
fn pax_value(data: &[u8], key: &str) -> Option<String> {
    String::from_utf8_lossy(data).lines().find_map(|record| {
        let (_, entry) = record.split_once(' ')?;
        let (name, value) = entry.split_once('=')?;
        (name == key).then(|| value.to_owned())
    })
}

#[cfg(test)]
#[path = "package_tests.rs"]
mod tests;
