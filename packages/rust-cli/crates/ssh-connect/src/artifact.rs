//! 本机的产物与远端的目标平台（remote-exec.md §19.9、§28）：产物的头（ELF 或 Mach-O）说明它是哪个平台（上传前核对，
//! 避免传错），sha256 给远端校验。
//!
//! F107（§28.2）：目标平台 = `uname -s` + `uname -m`，Linux x86_64 / aarch64 之外加 macOS arm64；Intel Mac 认得出来但
//! 没有可下载的包（[`Platform::published`] 为假）。

use std::io;
use std::path::{Path, PathBuf};

use rust_cli_base::units;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

/// 读产物的头时最多读这么多字节：universal 的 Mach-O 片表（最多 16 片、每片 32 字节）也在里面（§28.4）。
const HEAD: usize = 1024;
/// universal 的 Mach-O 最多认这么多片（也把别的格式的 `0xcafebabe` 挡住）。
const MAX_SLICES: u32 = 16;
/// 算 sha256 时每次读的字节数。
const HASH_BUFFER_BYTES: usize = 256 * units::KIB;

// ELF 头（System V ABI 的 `Elf64_Ehdr`）。
/// 认平台要读到的字节数（到 `e_machine` 为止）。
const ELF_HEAD_BYTES: usize = 20;
/// `e_ident[EI_CLASS]` 的偏移与 64 位的值 `ELFCLASS64`。
const ELF_CLASS_AT: usize = 4;
const ELF_CLASS_64: u8 = 2;
/// `e_ident[EI_DATA]` 的偏移与小端的值 `ELFDATA2LSB`。
const ELF_DATA_AT: usize = 5;
const ELF_DATA_LITTLE: u8 = 1;
/// `e_machine`（2 字节，按 `EI_DATA` 是小端）的偏移。
const ELF_MACHINE_AT: usize = 18;
/// `e_machine` 的 `EM_X86_64`。
const EM_X86_64: u16 = 62;
/// `e_machine` 的 `EM_AARCH64`。
const EM_AARCH64: u16 = 183;

// Mach-O 头（`mach-o/loader.h`、`mach-o/fat.h`）。
/// `cputype` 的 `CPU_TYPE_X86_64`。
const CPU_TYPE_X86_64: u32 = 0x0100_0007;
/// `cputype` 的 `CPU_TYPE_ARM64`。
const CPU_TYPE_ARM64: u32 = 0x0100_000c;
/// 64 位瘦包的魔数 `MH_MAGIC_64`（按小端读）。
const MH_MAGIC_64: u32 = 0xfeed_facf;
/// universal 包的魔数 `FAT_MAGIC`（大端），片表每片是 [`FAT_ARCH_BYTES`] 字节。
const FAT_MAGIC: u32 = 0xcafe_babe;
/// universal 包的魔数 `FAT_MAGIC_64`（大端），片表每片是 [`FAT_ARCH_64_BYTES`] 字节。
const FAT_MAGIC_64: u32 = 0xcafe_babf;
/// `fat_arch` 的字节数。
const FAT_ARCH_BYTES: usize = 20;
/// `fat_arch_64` 的字节数。
const FAT_ARCH_64_BYTES: usize = 32;
/// 头里一个字的字节数（魔数、`cputype`、片数都是 32 位）。
const MACH_WORD_BYTES: usize = size_of::<u32>();
/// 瘦包的 `cputype` 的偏移（紧跟魔数）。
const MH_CPUTYPE_AT: usize = MACH_WORD_BYTES;
/// universal 包的片数 `nfat_arch` 的偏移（紧跟魔数）。
const FAT_NARCH_AT: usize = MACH_WORD_BYTES;
/// universal 包的片表的起点（`fat_header` 的字节数：魔数与片数）。
const FAT_ARCHS_AT: usize = 2 * MACH_WORD_BYTES;

/// 远端支持的目标平台（§28.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    LinuxX86_64,
    LinuxAarch64,
    MacosAarch64,
    /// Intel Mac：探测认得出来，但没有发布的平台包（§28.2）。
    MacosX86_64,
}

impl Platform {
    /// `uname -s` / `uname -m` 的结果；认不出来时 None（Windows、FreeBSD 等照旧报 `Unsupported`，§28.2）。
    pub fn of(os: &str, machine: &str) -> Option<Self> {
        match (os, machine) {
            ("Linux", "x86_64" | "amd64") => Some(Self::LinuxX86_64),
            ("Linux", "aarch64" | "arm64") => Some(Self::LinuxAarch64),
            ("Darwin", "aarch64" | "arm64") => Some(Self::MacosAarch64),
            ("Darwin", "x86_64") => Some(Self::MacosX86_64),
            _ => None,
        }
    }

    /// 是 macOS 吗（试跑被系统杀掉时按签名报错，§28.5；探测跳过 libc 的判定，§28.3）。
    pub fn is_macos(self) -> bool {
        matches!(self, Self::MacosAarch64 | Self::MacosX86_64)
    }

    /// npm 平台包的 id（ZCode-rs 的 `platforms.json`，§28.2）：`darwin-x64` 没有发布，只用来写错误。
    pub fn package_id(self) -> &'static str {
        match self {
            Self::LinuxX86_64 => "linux-x64-gnu",
            Self::LinuxAarch64 => "linux-arm64-gnu",
            Self::MacosAarch64 => "darwin-arm64",
            Self::MacosX86_64 => "darwin-x64",
        }
    }

    /// 这个平台有发布的包吗（§28.2）：Intel Mac 没有，要上传时报 `NoPackage`。
    pub fn published(self) -> bool {
        self != Self::MacosX86_64
    }

    /// 下载与本机缓存的 target，也是包里 `build.json` 的 `target`（§28.2、§28.8）。
    pub fn triple(self) -> &'static str {
        match self {
            Self::LinuxX86_64 => "x86_64-unknown-linux-gnu",
            Self::LinuxAarch64 => "aarch64-unknown-linux-gnu",
            Self::MacosAarch64 => "aarch64-apple-darwin",
            Self::MacosX86_64 => "x86_64-apple-darwin",
        }
    }

    /// 手动放的静态产物的目录（linux.md §1 的 musl 产物）；macOS 没有这种区别（§28.8）。
    pub fn static_triple(self) -> Option<&'static str> {
        match self {
            Self::LinuxX86_64 => Some("x86_64-unknown-linux-musl"),
            Self::LinuxAarch64 => Some("aarch64-unknown-linux-musl"),
            Self::MacosAarch64 | Self::MacosX86_64 => None,
        }
    }

    /// 报 `NoArtifact` 时写的 target：有静态产物的目录就写它，没有的写下载的 target（§28.8）。
    pub(crate) fn missing_triple(self) -> &'static str {
        self.static_triple().unwrap_or_else(|| self.triple())
    }

    /// 两个 Linux 的平台（cli 列本机静态产物的目录用，§28.8）。
    pub const LINUX: [Self; 2] = [Self::LinuxX86_64, Self::LinuxAarch64];

    /// 这个产物能在这个平台上跑吗（§28.4）：Linux 看 ELF 头，macOS 看 Mach-O 的 `cputype`（universal 的包里有一片
    /// 对得上就算，整包上传：内核自己挑片）。认不出来的头一律为假。
    pub fn runs_image(self, head: &[u8]) -> bool {
        match self {
            Self::LinuxX86_64 | Self::LinuxAarch64 => Self::from_elf(head) == Some(self),
            Self::MacosAarch64 | Self::MacosX86_64 => self
                .cpu_type()
                .is_some_and(|cpu| mach_cpus(head).contains(&cpu)),
        }
    }

    /// ELF 头（前 20 字节）：64 位、小端、`e_machine` 是哪种架构。
    fn from_elf(head: &[u8]) -> Option<Self> {
        let head = head.get(..ELF_HEAD_BYTES)?;
        if &head[..4] != b"\x7fELF"
            || head[ELF_CLASS_AT] != ELF_CLASS_64
            || head[ELF_DATA_AT] != ELF_DATA_LITTLE
        {
            return None;
        }
        let machine = u16::from_le_bytes([head[ELF_MACHINE_AT], head[ELF_MACHINE_AT + 1]]);
        Self::LINUX
            .into_iter()
            .find(|platform| platform.elf_machine() == Some(machine))
    }

    /// ELF 头的 `e_machine`（macOS 的平台没有）。
    fn elf_machine(self) -> Option<u16> {
        match self {
            Self::LinuxX86_64 => Some(EM_X86_64),
            Self::LinuxAarch64 => Some(EM_AARCH64),
            Self::MacosAarch64 | Self::MacosX86_64 => None,
        }
    }

    /// Mach-O 的 `cputype`（Linux 的平台没有）。
    fn cpu_type(self) -> Option<u32> {
        match self {
            Self::MacosX86_64 => Some(CPU_TYPE_X86_64),
            Self::MacosAarch64 => Some(CPU_TYPE_ARM64),
            Self::LinuxX86_64 | Self::LinuxAarch64 => None,
        }
    }
}

/// Mach-O 的头里有哪些 `cputype`（§28.4）：64 位的瘦包（`0xfeedfacf`，偏移 4 是 `cputype`）给一个；universal 的片表
/// （`0xcafebabe` 每片 20 字节、`0xcafebabf` 每片 32 字节，都是大端）给每一片的。不是 Mach-O、或片表没有完整落在读到
/// 的头里时给空（别的格式也用 `0xcafebabe`，片数的上限把它们挡住）。
fn mach_cpus(head: &[u8]) -> Vec<u32> {
    let word = |at: usize, big: bool| {
        let bytes: [u8; MACH_WORD_BYTES] = head.get(at..at + MACH_WORD_BYTES)?.try_into().ok()?;
        Some(if big {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        })
    };
    let Some(magic) = word(0, false) else {
        return Vec::new();
    };
    if magic == MH_MAGIC_64 {
        return word(MH_CPUTYPE_AT, false).into_iter().collect();
    }
    let entry = match word(0, true) {
        Some(FAT_MAGIC) => FAT_ARCH_BYTES,
        Some(FAT_MAGIC_64) => FAT_ARCH_64_BYTES,
        _ => return Vec::new(),
    };
    let Some(slices) = word(FAT_NARCH_AT, true).filter(|slices| (1..=MAX_SLICES).contains(slices))
    else {
        return Vec::new();
    };
    // 片表要完整落在读到的头里，否则不猜（别的格式的魔数多半在这里被挡下）。
    if head.len() < FAT_ARCHS_AT + entry * slices as usize {
        return Vec::new();
    }
    (0..slices)
        .map(|index| word(FAT_ARCHS_AT + entry * index as usize, true))
        .collect::<Option<Vec<u32>>>()
        .unwrap_or_default()
}

/// 读文件开头的 [`HEAD`] 字节（认平台用，[`Platform::runs_image`]）。
pub(crate) async fn image_head(path: &Path) -> Option<Vec<u8>> {
    let mut file = rust_cli_fs::open_read(path).await.ok()?;
    let mut head = Vec::new();
    let mut buffer = [0u8; HEAD];
    while head.len() < HEAD {
        let room = HEAD - head.len();
        match file.read(&mut buffer[..room]).await {
            Ok(0) => break,
            Ok(read) => head.extend_from_slice(&buffer[..read]),
            Err(_) => return None,
        }
    }
    Some(head)
}

/// 在候选文件里找这个平台能跑的产物（第一个存在且头对得上的）。
pub(crate) async fn locate(candidates: &[PathBuf], platform: Platform) -> Option<PathBuf> {
    for candidate in candidates {
        if image_head(candidate)
            .await
            .is_some_and(|head| platform.runs_image(&head))
        {
            return Some(candidate.clone());
        }
    }
    None
}

/// 文件的 sha256（十六进制，按块读）。
pub(crate) async fn sha256(path: &Path) -> io::Result<String> {
    let mut file = rust_cli_fs::open_read(path).await?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; HASH_BUFFER_BYTES];
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
