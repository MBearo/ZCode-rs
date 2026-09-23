//! Owned POSIX process-tree snapshot. No global process registry or background polling.
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeMap, time::Duration};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    time::Instant,
};

#[derive(Clone)]
struct Process {
    parent: i32,
    group: i32,
    started: String,
    zombie: bool,
}
type Table = BTreeMap<i32, Process>;

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
async fn snapshot() -> Result<Table> {
    // 只在终止路径探测；限制大小和查表时间，ps 自身由 kill_on_drop 回收。
    tokio::time::timeout(Duration::from_millis(500), async {
        let mut child = Command::new("/bin/ps")
            .args(["-A", "-o", "pid=,ppid=,pgid=,stat=,lstart="])
            .env("LC_ALL", "C")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut bytes = vec![];
        child
            .stdout
            .take()
            .unwrap()
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await?;
        ensure!(
            bytes.len() <= 2 * 1024 * 1024,
            "Process table exceeds budget"
        );
        ensure!(child.wait().await?.success(), "Process lookup failed");
        Ok(parse(std::str::from_utf8(&bytes)?))
    })
    .await
    .context("Process lookup timed out")?
}

struct Tree {
    processes: BTreeMap<i32, String>,
    groups: BTreeMap<i32, Option<String>>,
}
impl Tree {
    fn new(root: i32, table: &Table) -> Self {
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
    fn refresh(&mut self, table: &Table) {
        let mut children: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
        let mut members: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
        for (&pid, process) in table {
            children.entry(process.parent).or_default().push(pid);
            members.entry(process.group).or_default().push(pid);
        }
        let mut queue = std::collections::VecDeque::new();
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
        let mut seen = std::collections::BTreeSet::new();
        while let Some(pid) = queue.pop_front() {
            if pid <= 1 || !seen.insert(pid) {
                continue;
            }
            let process = &table[&pid];
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
    fn live(&self, table: &Table) -> bool {
        self.processes.iter().any(|(pid, identity)| {
            table
                .get(pid)
                .is_some_and(|p| &p.started == identity && !p.zombie)
        })
    }
    fn maybe_live(&self) -> bool {
        self.groups.keys().any(|group| alive(-*group))
            || self.processes.keys().any(|pid| alive(*pid))
    }
    async fn signal(&self, signal: i32, table: &Table) -> Result<()> {
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
fn alive(pid: i32) -> bool {
    (unsafe { libc::kill(pid, 0) == 0 })
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}
fn send(pid: i32, signal: i32) -> Result<()> {
    if unsafe { libc::kill(pid, signal) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(error).context("Cannot signal owned process")
}

pub(super) async fn terminate(child: &mut Child, pid: u32, graceful: bool) -> Result<()> {
    let root = pid as i32;
    if !graceful && !alive(-root) {
        return Ok(());
    }
    let began = Instant::now();
    let table = snapshot().await?;
    let mut tree = Tree::new(root, &table);
    if graceful {
        tree.signal(libc::SIGTERM, &table).await?;
        while began.elapsed() < Duration::from_millis(1500) {
            child.try_wait()?;
            if !tree.maybe_live() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    // 单次 killpg 与 fork 竞争会漏掉持有管道的后代。保留组所有权，直到当前快照
    // 确认没有活工作进程；SIGKILL 后的组长退出不是整个工具完成的替代事实。
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let table = snapshot().await?;
        tree.refresh(&table);
        if !tree.live(&table) {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "Owned Bash processes did not terminate"
        );
        tree.signal(libc::SIGKILL, &table).await?;
        child.try_wait()?;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    if child.id().is_some() {
        child.wait().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshots_keep_owned_groups_and_reject_reused_identities() {
        let first = parse(
            "10 1 10 S Mon Sep 21 00:00:00 2026\n11 10 11 S Mon Sep 21 00:00:00 2026\n12 11 11 S Mon Sep 21 00:00:00 2026\n99 1 99 S Mon Sep 21 00:00:00 2026",
        );
        let mut tree = Tree::new(10, &first);
        assert_eq!(tree.processes.len(), 3);
        assert!(!tree.groups.contains_key(&99));
        let next = parse(
            "11 1 11 S Mon Sep 21 00:00:00 2026\n13 1 10 S Mon Sep 21 00:00:01 2026\n14 11 11 S Mon Sep 21 00:00:01 2026\n12 1 12 S Mon Sep 21 00:01:00 2026",
        );
        tree.refresh(&next);
        assert!(tree.processes.contains_key(&13));
        assert!(tree.processes.contains_key(&14));
        assert_ne!(tree.processes[&12], next[&12].started);
        assert!(!tree.groups.contains_key(&12));
        assert!(!tree.owns_group(11, &parse("11 1 11 S Mon Sep 21 00:02:00 2026")));
        assert!(tree.owns_group(10, &next));
    }
}
