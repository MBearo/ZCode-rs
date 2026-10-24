//! 本机的 Linux 产物（remote-exec.md §19.9）：ELF 头说明它是哪种架构（上传前核对，避免传错），sha256 给远端校验。

use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

/// 远端支持的架构（只有 Linux）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
}

impl Arch {
    /// `uname -s` / `uname -m` 的结果；不支持时 None。
    pub fn of(os: &str, machine: &str) -> Option<Self> {
        if os != "Linux" {
            return None;
        }
        match machine {
            "x86_64" | "amd64" => Some(Self::X86_64),
            "aarch64" | "arm64" => Some(Self::Aarch64),
            _ => None,
        }
    }

    /// 静态 musl 产物的 target（linux.md §1）。
    pub fn triple(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64-unknown-linux-musl",
            Self::Aarch64 => "aarch64-unknown-linux-musl",
        }
    }

    /// ELF 头的 `e_machine`。
    fn elf_machine(self) -> u16 {
        match self {
            Self::X86_64 => 62,
            Self::Aarch64 => 183,
        }
    }
}

/// 读 ELF 头：64 位、小端、可执行或位置无关、`e_machine` 是哪种架构。不是 Linux 能跑的 ELF 时 None。
pub(crate) async fn elf_arch(path: &Path) -> Option<Arch> {
    let mut file = rust_cli_fs::open_read(path).await.ok()?;
    let mut head = [0u8; 20];
    file.read_exact(&mut head).await.ok()?;
    if &head[..4] != b"\x7fELF" || head[4] != 2 || head[5] != 1 {
        return None;
    }
    let machine = u16::from_le_bytes([head[18], head[19]]);
    [Arch::X86_64, Arch::Aarch64]
        .into_iter()
        .find(|arch| arch.elf_machine() == machine)
}

/// 在候选文件里找这种架构的产物（第一个存在且 ELF 头对得上的）。
pub(crate) async fn locate(candidates: &[PathBuf], arch: Arch) -> Option<PathBuf> {
    for candidate in candidates {
        if elf_arch(candidate).await == Some(arch) {
            return Some(candidate.clone());
        }
    }
    None
}

/// 文件的 sha256（十六进制，按块读）。
pub(crate) async fn sha256(path: &Path) -> io::Result<String> {
    let mut file = rust_cli_fs::open_read(path).await?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
