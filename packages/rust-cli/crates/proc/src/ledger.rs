// 复制自 rust/crates/proc/src/ledger.rs（feat/rs 79b6691c55）。
//! 工具进程树的启动入口与账本（docs/specs/rust-tool-process-reaping.md）。
//!
//! runtime 启动的、自成会话或进程组的进程树（Bash 工具、hooks、MCP stdio 服务）只经
//! [`Ledger::spawn`] 启动：它隔离命令、启动根进程，并在 Unix 上写下一条记录（根进程与所有者的 pid
//! 和启动时间、runtime 实例）。[`Tracked`] 确认整棵树结束后删除记录；没有确认就被丢弃时记录留下，
//! 下一个 runtime 实例启动时 [`Ledger::sweep`] 处理它——所有者已不在而根进程仍是记录的那个时，
//! 结束整棵树。Windows 上不写记录，树放进随本进程结束的 Job。

use std::io;
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::process::{Child, Command};

use crate::tree::Shutdown;

#[cfg(unix)]
mod identity;
#[cfg(unix)]
mod record;
#[cfg(unix)]
mod sweep;
#[cfg(windows)]
mod windows;

/// 进程树的用途，只用于记录名与日志。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Bash,
    Hook,
    Mcp,
}

impl Purpose {
    fn as_str(self) -> &'static str {
        match self {
            Purpose::Bash => "bash",
            Purpose::Hook => "hook",
            Purpose::Mcp => "mcp",
        }
    }

    #[cfg(unix)]
    fn parse(name: &str) -> Option<Self> {
        [Purpose::Bash, Purpose::Hook, Purpose::Mcp]
            .into_iter()
            .find(|purpose| purpose.as_str() == name)
    }
}

/// Unix 上根进程怎样脱离 runtime；进程组 ID 都等于根 pid。Windows 上不起作用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    /// 新会话与新进程组（`setsid`，与 Node `spawn({detached: true})` 一致），没有控制终端：
    /// 读取 `/dev/tty` 的提示立即失败（B7）。Bash 工具与 hooks。
    Session,
    /// 只建新进程组（`setpgid(0, 0)`），留在 runtime 的会话里。MCP stdio 服务。
    Group,
}

/// Windows 上进程树放进哪个 Job（都设置了 `KILL_ON_JOB_CLOSE`，本进程退出时整棵树结束）。Unix 上不起作用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Containment {
    /// 这棵树自己的 Job：[`Tracked::terminate`] 结束其中所有进程（包括父进程已退出的后代），
    /// [`Tracked`] 被丢弃时整棵树也结束。Bash 工具。
    OwnJob,
    /// 进程级 Job：只随本进程结束；[`Tracked::terminate`] 用 `taskkill /T /F`，根进程退出后留下的
    /// 进程在本进程存活期间继续运行。hooks（它们有意留下的后台进程不随 hook 结束）与 MCP stdio 服务。
    ProcessJob,
}

/// 一个 runtime 实例的账本。可克隆；最后一个克隆与它启动的最后一个 [`Tracked`] 都释放后实例注销，
/// 此后它留下的记录由下一次清扫处理。
/// Windows 上不写记录，没有状态。
#[derive(Clone)]
pub struct Ledger {
    #[cfg(unix)]
    inner: Arc<Inner>,
}

#[cfg(unix)]
struct Inner {
    #[cfg(unix)]
    dir: PathBuf,
    #[cfg(unix)]
    instance: identity::Instance,
    #[cfg(unix)]
    swept: tokio::sync::OnceCell<()>,
    /// 写记录失败只警告一次。
    #[cfg(unix)]
    warned: AtomicBool,
}

impl Ledger {
    /// `dir` 是账本目录（一棵树一个文件），用到时才创建。Windows 上不使用。
    pub fn open(dir: PathBuf) -> Self {
        #[cfg(unix)]
        {
            Self {
                inner: Arc::new(Inner {
                    dir,
                    instance: identity::Instance::register(),
                    swept: tokio::sync::OnceCell::new(),
                    warned: AtomicBool::new(false),
                }),
            }
        }
        #[cfg(windows)]
        {
            let _ = dir;
            Self {}
        }
    }

    /// 清扫账本：结束所有者已不在的树，删除已结束的树的记录（spec 2.2）。每个实例只清扫一次；
    /// 并发调用等待同一次清扫；被取消的清扫由下一次调用重新开始。结果写日志，失败不报错。
    pub async fn sweep(&self) {
        #[cfg(unix)]
        self.inner
            .swept
            .get_or_init(|| sweep::run(&self.inner.dir))
            .await;
    }

    /// 启动 `command` 作为一棵新进程树的根进程。先等本实例的清扫结束，再按 `isolation` 隔离、
    /// 启动、写记录（Unix），或按 `containment` 放进 Job（Windows）。根进程刚启动就已退出、或写
    /// 记录失败时不留记录，进程照常返回。
    ///
    /// 不要在 `command` 上另设 `process_group` 或 `pre_exec` 隔离。
    pub async fn spawn(
        &self,
        command: &mut Command,
        purpose: Purpose,
        isolation: Isolation,
        containment: Containment,
    ) -> io::Result<Tracked> {
        self.sweep().await;
        #[cfg(unix)]
        {
            let _ = containment;
            match isolation {
                Isolation::Session => crate::group::isolate(command),
                Isolation::Group => {
                    command.process_group(0);
                }
            }
            let child = command.spawn()?;
            let pid = child
                .id()
                .ok_or_else(|| io::Error::other("spawned child has no pid"))?;
            let record = self.record(pid, purpose).await.map(|path| Pending {
                path,
                _instance: self.inner.clone(),
            });
            Ok(Tracked {
                child,
                pid,
                record,
                confirmed: false,
            })
        }
        #[cfg(windows)]
        {
            let _ = isolation;
            let child = command.spawn()?;
            let pid = child
                .id()
                .ok_or_else(|| io::Error::other("spawned child has no pid"))?;
            let job = windows::contain(&child, (pid, purpose), containment);
            Ok(Tracked { child, pid, job })
        }
    }

    /// 写下 `pid` 这棵树的记录；读不到身份或写入失败时为 `None`。
    #[cfg(unix)]
    async fn record(&self, pid: u32, purpose: Purpose) -> Option<PathBuf> {
        let root = i32::try_from(pid).ok()?;
        let owner = identity::this_process().await?.clone();
        let identity::Probe::Running(start) = identity::probe(root).await else {
            // 根进程已经退出（或查不到）：没有需要清扫的树。
            return None;
        };
        let record = record::Record {
            purpose,
            root: identity::Process { pid: root, start },
            owner,
            instance: self.inner.instance.id(),
        };
        match record::create(&self.inner.dir, &record.name()).await {
            Ok(path) => Some(path),
            Err(error) => {
                if !self.inner.warned.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        event = "process_ledger.write_failed",
                        dir = %self.inner.dir.display(),
                        %error,
                        "Cannot record a tool process tree; it is not reaped if this process dies"
                    );
                }
                None
            }
        }
    }
}

/// 一条尚未确认的记录。它让写下记录的实例保持登记：只要 [`Tracked`] 还在，同一进程里其他实例的
/// 清扫就不会把这棵树当作无主的，即使创建它的 [`Ledger`] 的其他克隆都已释放。
#[cfg(unix)]
struct Pending {
    path: PathBuf,
    _instance: Arc<Inner>,
}

/// [`Ledger::spawn`] 启动的进程树：持有根进程与它的记录（Unix）或 Job（Windows）。
///
/// 没有经 [`Tracked::terminate`] 或 [`Tracked::release`] 确认就被丢弃时（任务或 runtime 实例被中止），
/// Unix 上向整个进程组发 KILL，记录留给下一次清扫删除；Windows 上由 Job 处理。
pub struct Tracked {
    child: Child,
    pid: u32,
    #[cfg(unix)]
    record: Option<Pending>,
    /// 已经由 [`Tracked::terminate`]（成功）或 [`Tracked::release`] 确认：丢弃时不结束进程组。release 时
    /// 根进程还没有被回收，其余进程也是有意保留的。
    #[cfg(unix)]
    confirmed: bool,
    #[cfg(windows)]
    job: Option<crate::job::Job>,
}

impl Tracked {
    pub fn child(&mut self) -> &mut Child {
        &mut self.child
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// 结束整棵树并回收根进程；错误表示无法确认回收，记录保留。Unix：[`crate::tree::terminate`]，
    /// 成功后删除记录。Windows：没有 TERM 阶段（`shutdown` 不起作用），有自己的 Job 时结束 Job，
    /// 否则 `taskkill /T /F`。
    pub async fn terminate(&mut self, shutdown: Shutdown) -> anyhow::Result<()> {
        #[cfg(unix)]
        {
            crate::tree::terminate(&mut self.child, self.pid, shutdown).await?;
            self.confirmed = true;
            self.forget().await;
            Ok(())
        }
        #[cfg(windows)]
        {
            let _ = shutdown;
            windows::terminate(&mut self.child, self.pid, self.job.as_ref()).await
        }
    }

    /// 根进程已退出并被回收，其余进程有意保留（hook 留下的后台进程）：删除记录。根进程还没有被
    /// 回收时什么都不做，记录留给清扫。
    pub async fn release(mut self) {
        #[cfg(unix)]
        {
            self.confirmed = true;
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                self.forget().await;
            }
        }
        #[cfg(windows)]
        let _ = &mut self;
    }

    #[cfg(unix)]
    async fn forget(&mut self) {
        let Some(Pending { path, .. }) = self.record.take() else {
            return;
        };
        if let Err(error) = record::remove(&path).await {
            tracing::warn!(
                event = "process_ledger.remove_failed",
                path = %path.display(),
                %error,
                "Cannot remove a process ledger record"
            );
        }
    }
}

/// 缺陷修复：任务被中止（例如 Host 等实例收口超时后中止它）时，`kill_on_drop` 只结束根进程，组内
/// 其余进程留下；根进程已不存在，下一次清扫也只能删除记录（规格 2.2：不向身份对不上的进程发信号）。
/// 丢弃时根进程还没有被回收，它的 pid 与进程组 id 都不会被复用，所以向整个进程组发 KILL 只会命中
/// 这棵树。已经离开进程组、自建会话的后代不在其中（与正常停止的边界相同）。
///
/// 缺陷修复 R2-17：原来用“记录还在”判断是否需要结束进程组；写记录失败（账本目录不可写）时记录一开始就是
/// 空的，被中止的任务留下整个进程组。现在只看是否已经确认（`confirmed`）。
#[cfg(unix)]
impl Drop for Tracked {
    fn drop(&mut self) {
        if self.confirmed || self.child.id().is_none() {
            return;
        }
        let Ok(group) = libc::pid_t::try_from(self.pid) else {
            return;
        };
        // SAFETY: killpg 只投递信号；根进程是本进程尚未回收的子进程，进程组 id 不会被复用。
        #[expect(unsafe_code, reason = "killpg(2) 是 libc 的 FFI")]
        let sent = unsafe { libc::killpg(group, libc::SIGKILL) } == 0;
        if !sent {
            tracing::debug!(
                event = "process_ledger.drop_kill_failed",
                group,
                error = %std::io::Error::last_os_error(),
                "Cannot kill the process group of a dropped tool process tree"
            );
        }
    }
}
