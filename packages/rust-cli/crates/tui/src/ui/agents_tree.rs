//! 子代理列表的树（docs/specs/rust-cli/tui-panels.md §1.2，修复线 F30）。
//!
//! 原来画成平铺列表，子代理再派生的子代理也平铺，看不出父子关系；现在按查看位置只画一棵“聚焦”的树。
//! 父子关系只从快照条目的 `owner` 算（agents 的 `Live` 是唯一来源），这里只有按查看位置与“收到快照以后过了多久”
//! 算出的纯函数，不另存状态。
use std::collections::{HashMap, HashSet};

use rust_cli_kernel_api::ids::AgentId;
use rust_cli_kernel_api::panels::{SubagentEntry, SubagentStatus};

/// 结束后还留在任务表里的时长。
pub const KEEP_MS: u64 = 30_000;

/// 列表里的一行（深度优先的顺序）。
#[derive(Debug, Clone, Copy)]
pub struct TreeRow<'a> {
    pub entry: &'a SubagentEntry,
    /// 沿有效父代理往上数的层数（不读 agents 的派生深度）。
    pub depth: usize,
    /// 有效父代理所在的行；顶层为 None。
    pub parent: Option<usize>,
    /// 后面还有同一有效父代理的行：连接符画 `├`，否则 `└`。
    pub more_siblings: bool,
    /// `(+N)` 的 N：没驱逐、没隐藏的后代数；查看链上的行为 0。
    pub descendants: usize,
}

/// 一份快照在某个查看位置、某个时刻的样子。
pub struct Forest<'a> {
    entries: &'a [SubagentEntry],
    by_id: HashMap<&'a AgentId, &'a SubagentEntry>,
    /// 收到快照以后过了多久（毫秒）。
    passed_ms: u64,
    /// 正在查看的子代理；看主会话时为 None。
    viewing: Option<&'a AgentId>,
}

/// 隐藏的已完成后台代理：已完成、后台、没停放。保活原因 `agent:` / `bash:` 对应本工程的停放；只带
/// `flag:idle-window`（缺省不开）或没有保活原因的已完成后台代理都是这一种（tui-panels.md §1.2）。
fn hidden(entry: &SubagentEntry) -> bool {
    entry.status == SubagentStatus::Completed && entry.background && !entry.parked
}

impl<'a> Forest<'a> {
    pub fn new(entries: &'a [SubagentEntry], passed_ms: u64, viewing: Option<&'a AgentId>) -> Self {
        let by_id = entries.iter().map(|entry| (&entry.agent, entry)).collect();
        Self {
            entries,
            by_id,
            passed_ms,
            viewing,
        }
    }

    /// 驱逐（到期后从任务表删掉）：不在跑、没停放、没在查看，并且结束了 30 秒以上。停放的不驱逐（带 keepalive
    /// 时没有期限）；正在查看的不驱逐（查看时保留）。
    pub fn evicted(&self, entry: &SubagentEntry) -> bool {
        entry.status != SubagentStatus::Running
            && !entry.parked
            && self.viewing != Some(&entry.agent)
            && entry
                .ended_ago_ms
                .is_none_or(|ago| ago + self.passed_ms >= KEEP_MS)
    }

    /// 有效父代理：owner 不在快照里或已驱逐 → 顶层；owner 是隐藏的已完成后台代理 → 跳过它看它的 owner。
    fn parent(&self, entry: &SubagentEntry) -> Option<&'a SubagentEntry> {
        let mut seen = HashSet::new();
        let mut owner = entry.owner.as_ref();
        while let Some(id) = owner {
            if !seen.insert(id) {
                return None;
            }
            let found = *self.by_id.get(id)?;
            if self.evicted(found) {
                return None;
            }
            if !hidden(found) {
                return Some(found);
            }
            owner = found.owner.as_ref();
        }
        None
    }

    fn parent_id(&self, entry: &SubagentEntry) -> Option<&'a AgentId> {
        self.parent(entry).map(|parent| &parent.agent)
    }

    /// 查看链：正在查看的子代理与它的有效祖先；看主会话（或它已不在快照里）时为空。
    fn chain(&self) -> Vec<&'a AgentId> {
        let mut chain = Vec::new();
        let mut at = self.viewing.and_then(|id| self.by_id.get(id).copied());
        while let Some(entry) = at {
            if chain.contains(&&entry.agent) {
                break;
            }
            chain.push(&entry.agent);
            at = self.parent(entry);
        }
        chain
    }

    /// 深度：沿有效父代理往上数。
    fn depth(&self, entry: &SubagentEntry) -> usize {
        let mut seen = HashSet::new();
        let mut depth = 0;
        let mut at = self.parent(entry);
        while let Some(parent) = at {
            if !seen.insert(&parent.agent) {
                break;
            }
            depth += 1;
            at = self.parent(parent);
        }
        depth
    }

    /// `(+N)`：有效祖先里有 `agent` 的条目数（没驱逐、没隐藏）。
    fn descendants(&self, agent: &AgentId) -> usize {
        self.entries
            .iter()
            .filter(|entry| !self.evicted(entry) && !hidden(entry))
            .filter(|entry| {
                let mut seen = HashSet::new();
                let mut at = self.parent(entry);
                while let Some(parent) = at {
                    if &parent.agent == agent {
                        return true;
                    }
                    if !seen.insert(&parent.agent) {
                        return false;
                    }
                    at = self.parent(parent);
                }
                false
            })
            .count()
    }

    /// 可见的行：没驱逐；没隐藏或在查看链上；并且是查看对象的直接子代理、与它同一个有效父代理（看主会话
    /// 时就是顶层），或在查看链上。按有效父代理分组、组内按启动顺序（快照顺序），深度优先展开。
    pub fn rows(&self) -> Vec<TreeRow<'a>> {
        let chain = self.chain();
        let viewed = self.viewing.and_then(|id| self.by_id.get(id).copied());
        let target = viewed.map(|entry| &entry.agent);
        let target_parent = viewed.and_then(|entry| self.parent_id(entry));
        let visible: Vec<(&'a SubagentEntry, Option<&'a AgentId>)> = self
            .entries
            .iter()
            .map(|entry| (entry, self.parent_id(entry)))
            .filter(|(entry, parent)| {
                let linked = chain.contains(&&entry.agent);
                !self.evicted(entry)
                    && (!hidden(entry) || linked)
                    && (*parent == target || *parent == target_parent || linked)
            })
            .collect();
        let shown: HashSet<&AgentId> = visible.iter().map(|(entry, _)| &entry.agent).collect();
        // 父不在可见集合里时算顶层组。
        let mut groups: HashMap<Option<&AgentId>, Vec<&'a SubagentEntry>> = HashMap::new();
        for (entry, parent) in &visible {
            let key = parent.filter(|parent| shown.contains(parent));
            groups.entry(key).or_default().push(entry);
        }
        let mut order = Vec::new();
        let mut stack: Vec<&'a SubagentEntry> = groups
            .get(&None)
            .map(|top| top.iter().rev().copied().collect())
            .unwrap_or_default();
        let mut placed = HashSet::new();
        while let Some(entry) = stack.pop() {
            if !placed.insert(&entry.agent) {
                continue;
            }
            order.push(entry);
            if let Some(children) = groups.get(&Some(&entry.agent)) {
                stack.extend(children.iter().rev().copied());
            }
        }
        let parents: Vec<Option<&AgentId>> = order.iter().map(|e| self.parent_id(e)).collect();
        order
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let parent = parents[index];
                TreeRow {
                    entry,
                    depth: self.depth(entry),
                    parent: parent.and_then(|p| order.iter().position(|e| &e.agent == p)),
                    more_siblings: parents[index + 1..].contains(&parent),
                    descendants: if chain.contains(&&entry.agent) {
                        0
                    } else {
                        self.descendants(&entry.agent)
                    },
                }
            })
            .collect()
    }

    /// footer 的 `/tasks to see subagents`：有隐藏的已完成后台代理（没驱逐、不在查看链上）。
    pub fn has_hidden(&self) -> bool {
        let chain = self.chain();
        self.entries
            .iter()
            .any(|entry| hidden(entry) && !self.evicted(entry) && !chain.contains(&&entry.agent))
    }
}
