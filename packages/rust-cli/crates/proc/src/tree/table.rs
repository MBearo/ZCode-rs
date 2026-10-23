// 复制自 rust/crates/proc/src/tree/table.rs（feat/rs 79b6691c55）。
//! 进程表快照与按身份追踪的进程树（自 apps/zcode-cli-rust/crates/tools/src/process_tree.rs 移入，行为不变）。
//! 没有全局进程注册表，也没有后台轮询。

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use tokio::io::AsyncReadExt;

use super::posix::{alive, send};

/// 一次快照中进程的身份：启动时间（`lstart`）与 pid 一起区分 pid 复用。
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

/// `/bin/ps` 给出的进程表。
pub(super) async fn snapshot() -> Result<Table> {
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
