//! Bounded extraction of a plugin ZIP (Node `zip-source.ts`
//! `extractZipArchive`, `classifyZipEntry`, `normalizeZipRelativePath`, with
//! yauzl 3's file name checks). Runs on a blocking thread. Spec
//! rust-m10-4-plugin-sources §9.
use anyhow::{Result, anyhow, bail};
use std::io::Read;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

const MAX_ENTRIES: usize = 20_000;
const MAX_EXTRACTED_BYTES: u64 = 500 * 1024 * 1024;
pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
const FILE_TYPE_MASK: u32 = 0o170000;
const REGULAR: u32 = 0o100000;
const DIRECTORY: u32 = 0o040000;
const SYMLINK: u32 = 0o120000;

fn unsafe_path(path: &str) -> anyhow::Error {
    anyhow!("Unsafe plugin zip path: {path}")
}

/// Node `normalizeZipRelativePath`.
pub fn normalize(path: &str) -> Result<String> {
    if path.contains('\0') {
        return Err(unsafe_path(path));
    }
    let trimmed = path.trim_end_matches('/');
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if trimmed.is_empty()
        || path.contains('\\')
        || path.starts_with('/')
        || Path::new(path).is_absolute()
        || drive
    {
        return Err(unsafe_path(path));
    }
    if trimmed
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(unsafe_path(path));
    }
    Ok(trimmed.to_owned())
}

/// Node `resolveZipPathWithin`.
pub fn within(root: &Path, relative: &str) -> Result<PathBuf> {
    let mut target = root.to_owned();
    target.extend(relative.split('/'));
    let target = crate::fsx::normalize(&target);
    match target.strip_prefix(crate::fsx::normalize(root)) {
        Ok(rest) if !rest.as_os_str().is_empty() => Ok(target),
        _ => Err(unsafe_path(relative)),
    }
}

/// yauzl `validateFileName` after its non-strict backslash conversion.
fn yauzl_name(raw: &str) -> Result<String> {
    let name = raw.replace('\\', "/");
    let bytes = name.as_bytes();
    let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if drive || name.starts_with('/') {
        bail!("absolute path: {name}");
    }
    if name.split('/').any(|part| part == "..") {
        bail!("invalid relative path: {name}");
    }
    Ok(name)
}

/// Extracts `archive` into `target`; returns the top-level segments in
/// first-seen order.
pub fn extract(archive: &Path, target: &Path, cancel: &CancellationToken) -> Result<Vec<String>> {
    std::fs::create_dir_all(target)?;
    let mut zip = zip::ZipArchive::new(std::fs::File::open(archive)?)?;
    let mut top_level: Vec<String> = vec![];
    let mut extracted: u64 = 0;
    for index in 0..zip.len() {
        crate::failure::check(cancel)?;
        let count = index + 1;
        if count > MAX_ENTRIES {
            bail!("Plugin zip has too many entries: {count}/{MAX_ENTRIES}");
        }
        let (name, encrypted, mode, size, method) = {
            let entry = zip.by_index_raw(index)?;
            (
                yauzl_name(entry.name())?,
                entry.encrypted(),
                entry.unix_mode().unwrap_or(0),
                entry.size(),
                entry.compression(),
            )
        };
        let normalized = normalize(&name)?;
        let segment = normalized
            .split('/')
            .next()
            .unwrap_or(&normalized)
            .to_owned();
        if !top_level.contains(&segment) {
            top_level.push(segment);
        }
        let path = within(target, &normalized)?;
        if encrypted {
            bail!("Encrypted plugin zip entries are not supported: {name}");
        }
        let kind = mode & FILE_TYPE_MASK;
        if kind == SYMLINK {
            bail!("Plugin zip entry symlinks are not supported: {name}");
        }
        if kind != 0 && kind != REGULAR && kind != DIRECTORY {
            bail!("Unsupported plugin zip entry type: {name}");
        }
        if kind == DIRECTORY || name.ends_with('/') {
            std::fs::create_dir_all(&path)?;
            continue;
        }
        if size > MAX_FILE_BYTES {
            bail!("Plugin zip entry exceeds single file limit: {name}");
        }
        #[allow(deprecated)]
        match method {
            zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated => {}
            zip::CompressionMethod::Unsupported(code) => {
                bail!("unsupported compression method: {code}")
            }
            other => bail!("unsupported compression method: {other:?}"),
        }
        let mut bytes = vec![];
        zip.by_index(index)?
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            bail!("Plugin zip entry exceeds single file limit: {name}");
        }
        extracted += bytes.len() as u64;
        if extracted > MAX_EXTRACTED_BYTES {
            bail!("Plugin zip extracted content exceeds limit: {extracted}/{MAX_EXTRACTED_BYTES}");
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, bytes)?;
    }
    Ok(top_level)
}

#[cfg(test)]
#[path = "zip_extract_tests.rs"]
mod tests;
