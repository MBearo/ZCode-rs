// 复制自 rust/crates/proc/src/ledger/identity.rs（feat/rs 79b6691c55）。
//! 进程的身份（pid 与操作系统给出的启动时间）与进程内 runtime 实例的登记表
//! （docs/specs/rust-tool-process-reaping.md 2.1）。

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use tokio::sync::OnceCell;

/// 操作系统给出的进程启动时间，只做相等比较。只含十六进制数字与 `.`，不含记录名的分隔符 `-`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Start(String);

impl Start {
    /// 记录名中的启动时间字段；格式不对时为 `None`。
    pub(super) fn parse(field: &str) -> Option<Self> {
        let valid = !field.is_empty()
            && field
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'.');
        valid.then(|| Self(field.to_owned()))
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

/// 一个进程：pid 与启动时间同时相符才是同一个进程。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Process {
    pub(super) pid: i32,
    pub(super) start: Start,
}

/// 按 pid 查到的进程。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Probe {
    /// 正在运行，启动时间如上。
    Running(Start),
    /// 不存在，或已经退出、只剩等待回收的僵尸进程。
    Gone,
    /// 查询失败：既不能说它在运行，也不能说它已结束。
    Unknown,
}

impl Probe {
    /// 正是记录的那个进程，且仍在运行。
    pub(super) fn is(&self, start: &Start) -> bool {
        matches!(self, Probe::Running(running) if running == start)
    }
}

/// 本进程；读不到自己的启动时间时为 `None`（此时不写记录）。
pub(super) async fn this_process() -> Option<&'static Process> {
    static THIS: OnceCell<Option<Process>> = OnceCell::const_new();
    THIS.get_or_init(|| async {
        let pid = i32::try_from(std::process::id()).ok()?;
        match probe(pid).await {
            Probe::Running(start) => Some(Process { pid, start }),
            Probe::Gone | Probe::Unknown => None,
        }
    })
    .await
    .as_ref()
}

/// macOS：`proc_pidinfo(PROC_PIDTBSDINFO)` 的启动时刻，微秒。
#[cfg(target_os = "macos")]
#[expect(unsafe_code, reason = "proc_pidinfo 没有安全封装")]
pub(super) async fn probe(pid: i32) -> Probe {
    if pid <= 0 {
        return Probe::Gone;
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let Ok(size) = libc::c_int::try_from(size_of::<libc::proc_bsdinfo>()) else {
        return Probe::Unknown;
    };
    // SAFETY: 缓冲区大小与 PROC_PIDTBSDINFO 对应的结构体一致；proc_pidinfo 最多写入 size 字节，
    // 返回实际写入的字节数，不保留指针。
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        let gone =
            written <= 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        return if gone { Probe::Gone } else { Probe::Unknown };
    }
    // SAFETY: 写入的字节数等于结构体大小，整个结构体都已初始化（字段都是整数与字节数组）。
    let info = unsafe { info.assume_init() };
    if info.pbi_status == libc::SZOMB {
        return Probe::Gone;
    }
    let micros = info
        .pbi_start_tvsec
        .checked_mul(rust_cli_base::units::MICROS_PER_SECOND)
        .and_then(|micros| micros.checked_add(info.pbi_start_tvusec));
    match micros {
        Some(micros) => Probe::Running(Start(format!("{micros:x}"))),
        None => Probe::Unknown,
    }
}

/// Linux：`/proc/<pid>/stat` 的启动 tick（开机以来），前面加开机标识，重启后不会与旧记录相符。
// 缺陷修复（L21 三平台验收）：这两处读 /proc 只在 Linux 编译，macOS 上的 clippy 看不到；Linux 上
// 被工作区的 `tokio::fs` 禁令（本机存储只经 rust-cli-fs）拦下，`check:rust-cli` 编译失败。
// /proc 是内核的进程信息伪文件，不是本机存储；按 README §3.2 在 proc crate 的具体函数上豁免。
#[cfg(target_os = "linux")]
#[expect(
    clippy::disallowed_methods,
    reason = "/proc 是进程信息伪文件，不是本机存储（README §3.2 的 proc 函数级豁免）"
)]
pub(super) async fn probe(pid: i32) -> Probe {
    if pid <= 0 {
        return Probe::Gone;
    }
    use rust_cli_base::proto::procfs;

    let stat = match tokio::fs::read_to_string(procfs::stat_path(pid)).await {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Probe::Gone,
        Err(_) => return Probe::Unknown,
    };
    // comm 可能含空格与括号：base 的 `procfs::field` 从最后一个 ')' 之后数字段。
    let Some(state) = procfs::field(&stat, procfs::STATE) else {
        return Probe::Unknown;
    };
    if matches!(state, "Z" | "X" | "x") {
        return Probe::Gone;
    }
    match procfs::field(&stat, procfs::STARTTIME).and_then(|ticks| ticks.parse::<u64>().ok()) {
        Some(ticks) => Probe::Running(Start(format!("{}.{ticks:x}", boot().await))),
        None => Probe::Unknown,
    }
}

/// 本次开机的标识（`boot_id` 去掉连字符）；读不到时为 `0`（只比较 tick）。
#[cfg(target_os = "linux")]
#[expect(
    clippy::disallowed_methods,
    reason = "/proc 是进程信息伪文件，不是本机存储（README §3.2 的 proc 函数级豁免）"
)]
async fn boot() -> &'static str {
    static BOOT: OnceCell<String> = OnceCell::const_new();
    BOOT.get_or_init(|| async {
        let id = tokio::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .await
            .unwrap_or_default();
        let id: String = id.chars().filter(char::is_ascii_hexdigit).collect();
        if id.is_empty() { "0".to_owned() } else { id }
    })
    .await
}

/// 其他 Unix：读不到启动时间，不写记录，也不按记录结束任何进程。
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) async fn probe(pid: i32) -> Probe {
    let _ = pid;
    Probe::Unknown
}

/// 本进程中仍在运行的 runtime 实例。只做单次插入与删除，中毒时继续使用（rust-u1-runtime-library.md 6.2）。
static LIVE: LazyLock<Mutex<HashSet<u64>>> = LazyLock::new(Default::default);
static NEXT: AtomicU64 = AtomicU64::new(1);

fn live() -> MutexGuard<'static, HashSet<u64>> {
    LIVE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 一个 runtime 实例的登记：创建时登记，释放时注销。本进程写下的记录，实例已注销就是无主的。
#[derive(Debug)]
pub(super) struct Instance(u64);

impl Instance {
    pub(super) fn register() -> Self {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        live().insert(id);
        Self(id)
    }

    pub(super) fn id(&self) -> u64 {
        self.0
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        live().remove(&self.0);
    }
}

/// 编号为 `id` 的实例仍在本进程中运行。
pub(super) fn registered(id: u64) -> bool {
    live().contains(&id)
}
