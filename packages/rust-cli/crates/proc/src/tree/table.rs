// 复制自 rust/crates/proc/src/tree/table.rs（feat/rs 79b6691c55）。
//! 进程表快照与按身份追踪的进程树（自 apps/zcode-cli-rust/crates/tools/src/process_tree.rs 移入，行为不变）。
//! 没有全局进程注册表，也没有后台轮询。
//!
//! 远端执行 F98（docs/specs/rust-cli/remote-exec.md §25.3）：Linux 上进程表读 `/proc`，不依赖 `ps`。

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use tokio::io::AsyncReadExt;

use super::posix::{alive, send};

/// 一次快照中进程的身份：启动时间（`ps` 的 `lstart`，Linux 上是 `/proc` 的 starttime）与 pid 一起区分 pid 复用。
#[derive(Clone)]
pub(super) struct Process {
    parent: i32,
    group: i32,
    started: String,
    zombie: bool,
}

pub(super) type Table = BTreeMap<i32, Process>;

const SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(500);
const SNAPSHOT_MAX_BYTES: usize = 2 * 1024 * 1024;

fn parse(text: &str) -> Table {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let parent = fields.next()?.parse().ok()?;
            let group = fields.next()?.parse().ok()?;
            let zombie = fields.next()?.starts_with('Z');
            let started = fields.collect::<Vec<_>>().join(" ");
            (!started.is_empty()).then_some((
                pid,
                Process {
                    parent,
                    group,
                    started,
                    zombie,
                },
            ))
        })
        .collect()
}

/// `/proc/<pid>/stat` 的一行：（pid，进程）。comm 可能有空格与括号，从最后一个 `)` 之后数：第 1 个字段是状态，
/// 第 2 个 ppid，第 3 个 pgrp，第 20 个 starttime（开机以来的 tick）。
#[cfg(any(target_os = "linux", test))]
pub(super) fn parse_stat(line: &str) -> Option<(i32, Process)> {
    let (head, rest) = line.rsplit_once(')')?;
    let pid = head.split_once('(')?.0.trim().parse().ok()?;
    let mut fields = rest.split_whitespace();
    let state = fields.next()?;
    let parent = fields.next()?.parse().ok()?;
    let group = fields.next()?.parse().ok()?;
    let started = fields.nth(16)?;
    started.parse::<u64>().ok()?;
    Some((
        pid,
        Process {
            parent,
            group,
            started: started.to_owned(),
            zombie: matches!(state, "Z" | "X" | "x"),
        },
    ))
}

/// 进程表的来源：本进程第一次取快照时选定，之后不换（两种来源的身份写法不同，混用会把同一个进程当成两个）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    #[cfg(target_os = "linux")]
    Proc,
    Ps,
}

#[cfg(target_os = "linux")]
async fn source() -> Source {
    static SOURCE: tokio::sync::OnceCell<Source> = tokio::sync::OnceCell::const_new();
    *SOURCE
        .get_or_init(|| async {
            if proc_readable().await {
                Source::Proc
            } else {
                tracing::warn!("/proc is not readable; the process table comes from /bin/ps");
                Source::Ps
            }
        })
        .await
}

#[cfg(not(target_os = "linux"))]
async fn source() -> Source {
    Source::Ps
}

/// 进程表快照（Linux 读 `/proc`，其余用 `/bin/ps`）。
///
/// 缺陷修复（Docker 试用 L2）：以前只用 `/bin/ps -A -o …,lstart=`。alpine 的 busybox `ps` 不认 `lstart`（退出码 1），
/// debian-slim 没有 `/bin/ps`：远端执行器每次结束进程树都取不到快照，退回只对根进程组发信号，自己另起进程组的后代
/// 留下来。Linux 的执行器常跑在这类精简的系统上，所以 Linux 上直接读 `/proc`。
pub(super) async fn snapshot() -> Result<Table> {
    match source().await {
        #[cfg(target_os = "linux")]
        Source::Proc => tokio::time::timeout(SNAPSHOT_TIMEOUT, proc_table())
            .await
            .context("Process lookup timed out")?,
        Source::Ps => ps_table().await,
    }
}

/// `/proc` 能读（没挂载或被挡住时退回 `/bin/ps`）。
#[cfg(target_os = "linux")]
#[expect(
    clippy::disallowed_methods,
    reason = "/proc 是进程信息伪文件，不是本机存储（README §3.2 的 proc 函数级豁免）"
)]
async fn proc_readable() -> bool {
    tokio::fs::read_to_string("/proc/self/stat")
        .await
        .is_ok_and(|stat| parse_stat(&stat).is_some())
}

/// 进程数的上限（同 `ps` 的输出预算，只防异常的系统）。
#[cfg(target_os = "linux")]
const PROC_MAX_PROCESSES: usize = 65_536;

/// 读 `/proc` 的进程表：每个数字目录的 `stat`；读的时候进程退出了就跳过。快照里没有本进程时算失败（`hidepid` 等）。
#[cfg(target_os = "linux")]
#[expect(
    clippy::disallowed_methods,
    reason = "/proc 是进程信息伪文件，不是本机存储（README §3.2 的 proc 函数级豁免）"
)]
pub(super) async fn proc_table() -> Result<Table> {
    let mut entries = tokio::fs::read_dir("/proc").await?;
    let mut table = Table::new();
    while let Some(entry) = entries.next_entry().await? {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        ensure!(
            table.len() < PROC_MAX_PROCESSES,
            "Process table exceeds budget"
        );
        let Ok(stat) = tokio::fs::read_to_string(format!("/proc/{pid}/stat")).await else {
            continue;
        };
        if let Some((_, process)) = parse_stat(&stat) {
            table.insert(pid, process);
        }
    }
    let me = i32::try_from(std::process::id()).context("Invalid process id")?;
    ensure!(table.contains_key(&me), "/proc does not show this process");
    Ok(table)
}

/// `/bin/ps` 给出的进程表。
async fn ps_table() -> Result<Table> {
    // 只在终止路径探测；限制大小和查表时间，ps 自身由 kill_on_drop 回收。
    tokio::time::timeout(SNAPSHOT_TIMEOUT, async {
        let mut child = crate::command("/bin/ps")
            .args(["-A", "-o", "pid=,ppid=,pgid=,stat=,lstart="])
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut bytes = vec![];
        child
            .stdout
            .take()
            .context("Missing process lookup output")?
            .take(SNAPSHOT_MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .await?;
        ensure!(
            bytes.len() <= SNAPSHOT_MAX_BYTES,
            "Process table exceeds budget"
        );
        ensure!(child.wait().await?.success(), "Process lookup failed");
        Ok(parse(std::str::from_utf8(&bytes)?))
    })
    .await
    .context("Process lookup timed out")?
}

/// 本次终止拥有的进程（pid → 启动时间）与进程组（pgid → 组长启动时间）。
pub(super) struct Tree {
    processes: BTreeMap<i32, String>,
    groups: BTreeMap<i32, Option<String>>,
}

impl Tree {
    pub(super) fn new(root: i32, table: &Table) -> Self {
        let mut tree = Self {
            processes: BTreeMap::new(),
            groups: BTreeMap::new(),
        };
        tree.groups
            .insert(root, table.get(&root).map(|p| p.started.clone()));
        if let Some(p) = table.get(&root) {
            tree.processes.insert(root, p.started.clone());
        }
        tree.refresh(table);
        tree
    }

    fn owns_group(&self, group: i32, table: &Table) -> bool {
        self.groups.get(&group).is_some_and(|identity| {
            // 新进程占用了已退出组长的 PID 时不能对其组发信号。无组长的旧组仍需清理。
            table
                .get(&group)
                .is_none_or(|p| identity.as_ref() == Some(&p.started))
        })
    }

    pub(super) fn refresh(&mut self, table: &Table) {
        let mut children: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
        let mut members: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
        for (&pid, process) in table {
            children.entry(process.parent).or_default().push(pid);
            members.entry(process.group).or_default().push(pid);
        }
        let mut queue = VecDeque::new();
        for (&pid, identity) in &self.processes {
            if table.get(&pid).is_some_and(|p| &p.started == identity) {
                queue.push_back(pid);
            }
        }
        for &group in self.groups.keys() {
            if self.owns_group(group, table)
                && let Some(pids) = members.get(&group)
            {
                queue.extend(pids);
            }
        }
        let mut seen = BTreeSet::new();
        while let Some(pid) = queue.pop_front() {
            if pid <= 1 || !seen.insert(pid) {
                continue;
            }
            let Some(process) = table.get(&pid) else {
                continue;
            };
            self.processes.insert(pid, process.started.clone());
            if let Some(pids) = children.get(&pid) {
                queue.extend(pids);
            }
            // 只有组长确认为本次后代，才能接管 job control / PTY 创建的整个组。
            if process.group == pid {
                self.groups.insert(pid, Some(process.started.clone()));
                if let Some(pids) = members.get(&pid) {
                    queue.extend(pids);
                }
            }
        }
    }

    /// 快照中还有本次拥有的、未成为僵尸的进程。
    pub(super) fn live(&self, table: &Table) -> bool {
        self.processes.iter().any(|(pid, identity)| {
            table
                .get(pid)
                .is_some_and(|p| &p.started == identity && !p.zombie)
        })
    }

    /// 不查进程表的粗略判断：任一拥有的组或 pid 仍可被发信号。
    pub(super) fn maybe_live(&self) -> bool {
        self.groups.keys().any(|group| alive(-*group))
            || self.processes.keys().any(|pid| alive(*pid))
    }

    pub(super) async fn signal(
        &self,
        signal: i32,
        table: &Table,
        snapshot: &impl AsyncFn() -> Result<Table>,
    ) -> Result<()> {
        for &group in self.groups.keys() {
            if self.owns_group(group, table)
                && table.values().any(|p| p.group == group && !p.zombie)
                && let Err(error) = send(-group, signal)
            {
                // MCP 断开 stdin 后可在 ps 与 killpg 之间退出；macOS 此时也可能返回 EPERM。
                // 仅当新快照证明原拥有者已退出才视为成功，不能吞掉仍存活进程的权限错误。
                let fresh = snapshot().await?;
                if self.owns_group(group, &fresh)
                    && fresh.values().any(|p| p.group == group && !p.zombie)
                {
                    return Err(error);
                }
            }
        }
        for (&pid, identity) in &self.processes {
            if table
                .get(&pid)
                .is_some_and(|p| &p.started == identity && !p.zombie)
                && let Err(error) = send(pid, signal)
            {
                let fresh = snapshot().await?;
                if fresh
                    .get(&pid)
                    .is_some_and(|p| &p.started == identity && !p.zombie)
                {
                    return Err(error);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "table_tests.rs"]
mod tests;
