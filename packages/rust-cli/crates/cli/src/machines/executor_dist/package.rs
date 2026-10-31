//! npm 平台包的纯计算部分（docs/specs/rust-cli/remote-exec.md §26.3、§26.4）：包名、`dist.integrity` 的校验、从 tarball
//! 解出执行器并核对 `build.json` 与产物的头（ELF 或 Mach-O，F107 §28.4）。不做 IO。

use std::io::Read;
use std::ops::Range;

use base64::Engine;
use rust_cli_base::units;
use rust_cli_ssh_connect::Platform;
use serde_json::Value;
use sha2::{Digest, Sha256, Sha512};

use crate::consts;

/// 平台包名的前缀（ZCode-rs 的发布脚本 `scripts/rust-cli/npm/package.mjs`：`@mbearo/zcode-<id>`）。
const PACKAGE_PREFIX: &str = "@mbearo/zcode-";
/// 包里的执行器与构建记录（发布脚本放的位置；npm 的 tarball 里每项都在 `package/` 下）。
const BINARY_ENTRY: &str = "package/bin/zcode-rs";
const BUILD_ENTRY: &str = "package/build.json";
/// 包里一项最多读这么多（执行器几十 MB；防止坏的 tar 头让内存失控）。
const MAX_ENTRY: u64 = 512 * units::MIB as u64;
const BLOCK: usize = 512;

// ── ustar 头的布局（POSIX pax / ustar：各字段在 512 字节的头里的字节范围）──

/// 文件名。
const NAME_FIELD: Range<usize> = 0..100;
/// 大小（八进制文本，或最高位置 1 的大端二进制）。
const SIZE_FIELD: Range<usize> = 124..136;
/// 类型标志（`0` 或 NUL 是普通文件，`x` 是 pax 扩展头）。
const TYPEFLAG: usize = 156;
/// 魔数 `ustar`（不含后面的 NUL 与版本号）。
const MAGIC_FIELD: Range<usize> = 257..262;
/// 文件名的前缀（`prefix/name`）。
const PREFIX_FIELD: Range<usize> = 345..500;
/// 大小字段的首字节最高位置 1 时，其余字节是大端的二进制（GNU 的 base-256 写法）。
const BINARY_SIZE_FLAG: u8 = 0x80;

/// 远端平台对应的平台包（§26.3、§28.2；id 由 ssh-connect 的 `Platform` 给）。
pub(super) fn package_name(platform: Platform) -> String {
    format!("{PACKAGE_PREFIX}{}", platform.package_id())
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

/// 能放进路径的版本号（F103，remote-exec.md §27.3：注册表的 latest 进本机缓存与远端的路径）：以数字开头、只含
/// `[0-9A-Za-z.+-]`、不含 `..`。
pub(super) fn plain_version(text: &str) -> bool {
    text.starts_with(|c: char| c.is_ascii_digit())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-'))
        && !text.contains("..")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 从 tarball（gzip + tar）解出执行器，核对 `build.json`（版本、target、sha256）与产物的头。错误是一句简短的原因。
pub(super) fn extract(
    tarball: &[u8],
    version: &str,
    platform: Platform,
) -> Result<Vec<u8>, String> {
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
    let target = platform.triple();
    if field("target") != target {
        return Err(format!(
            "build.json's target is {:?}, expected {target}",
            field("target")
        ));
    }
    if !field("sha256").eq_ignore_ascii_case(&hex(&Sha256::digest(&binary))) {
        return Err("build.json's sha256 does not match bin/zcode-rs".to_owned());
    }
    if !platform.runs_image(&binary) {
        return Err(format!("bin/zcode-rs is not a {target} executable"));
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
            matches!(header[TYPEFLAG], b'0' | 0) && (name == BINARY_ENTRY || name == BUILD_ENTRY);
        if header[TYPEFLAG] == b'x' || wanted {
            if size > MAX_ENTRY {
                return Err(invalid(format!("{name} is too large")));
            }
            let mut data = Vec::new();
            (&mut reader).take(size).read_to_end(&mut data)?;
            if data.len() as u64 != size {
                return Err(invalid(format!("{name} is cut short")));
            }
            skip(&mut reader, padding)?;
            match header[TYPEFLAG] {
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
    let field = &header[SIZE_FIELD];
    if field[0] & BINARY_SIZE_FLAG != 0 {
        return Ok(field[1..]
            .iter()
            .fold(0u64, |value, byte| (value << 8) | u64::from(*byte)));
    }
    let text = String::from_utf8_lossy(field);
    let text = text.trim_matches(|c: char| c == '\0' || c == ' ');
    if text.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(text, consts::OCTAL_RADIX)
        .map_err(|_| invalid(format!("bad tar size {text:?}")))
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
    let name = text(&header[NAME_FIELD]);
    let prefix = if &header[MAGIC_FIELD] == b"ustar" {
        text(&header[PREFIX_FIELD])
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
