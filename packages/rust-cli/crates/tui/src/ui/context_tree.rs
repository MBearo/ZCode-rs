//! 上下文的页的大纲（docs/specs/rust-cli/tui-panels.md §4.3）：顶层是 `/stats` 卡的五个组（kit `stats::groups` 的次序与
//! 档），下面是内核给的明细（`ContextUsage` 的各个列表）。行逐个打开；开合与选中按路径记（`组/名字/名字`），刷新后仍对得上。
//! 这里只有数据与走法，行的样子在 `context_page.rs`。
use std::collections::BTreeSet;

use rust_cli_kernel_api::context_usage::{CategoryKind, ContextUsage};
use rust_cli_tui_kit::app::widgets::stats;

/// 一行的路径：顶层是组的键（`messages`、`tools`…），下面是名字（规则文件是内核给的完整路径）。
pub type Path = Vec<String>;

/// 规则文件的路径怎样写（设计稿：`AGENTS.md`、`~/.zcode/AGENTS.md`）：工作区里的写相对路径，家目录写成 `~`。
#[derive(Debug, Clone, Copy, Default)]
pub struct Places<'a> {
    pub root: &'a str,
    pub home: Option<&'a str>,
}

impl Places<'_> {
    pub fn show(&self, path: &str) -> String {
        if !self.root.is_empty()
            && let Some(rest) = path.strip_prefix(self.root)
            && let Some(rest) = rest.strip_prefix(['/', '\\'])
        {
            return rest.to_owned();
        }
        super::app::label(path, self.home)
    }
}

/// 大纲的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// 路径里这一段（顶层是组的键，其余是名字）。
    pub key: String,
    pub name: String,
    pub note: String,
    pub tokens: u64,
    /// 安静的行（按需加载的，不算进合计）：全部 fg3，没有占比。
    pub quiet: bool,
    /// 指向的命令（`/mcp`、`/memory`、`/skills`）。
    pub pointer: Option<&'static str>,
    /// 顶层：组在卡的图例里的名次（kit `stats::step` 的档）。
    pub rank: Option<usize>,
    pub kids: Vec<Node>,
}

impl Node {
    fn leaf(name: impl Into<String>, note: impl Into<String>, tokens: u64) -> Self {
        let name = name.into();
        Self {
            key: name.clone(),
            name,
            note: note.into(),
            tokens,
            quiet: false,
            pointer: None,
            rank: None,
            kids: Vec::new(),
        }
    }

    fn with(mut self, kids: Vec<Node>) -> Self {
        self.kids = sorted(kids);
        self
    }
}

/// 一层的次序：大的在前（同量时按内核的次序，稳定排序），安静的行在最后。
fn sorted(mut kids: Vec<Node>) -> Vec<Node> {
    kids.sort_by(|a, b| a.quiet.cmp(&b.quiet).then(b.tokens.cmp(&a.tokens)));
    kids
}

/// `n 个东西`：1 时单数。
fn many(n: usize, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

/// 某个类别的 token（没有时 0）。
fn category(usage: &ContextUsage, name: &str) -> u64 {
    usage
        .categories
        .iter()
        .filter(|c| c.name == name)
        .map(|c| c.tokens)
        .sum()
}

/// 内核的类别 → kit 卡的来源（`stats::SOURCES` 按来源合并成 system、tools、MCP、skills、messages）。
pub fn source_of(category: &str) -> &'static str {
    match category {
        "System prompt" => "system_prompt",
        "Memory files" => "meta_user_context",
        "System tools" => "system_tool_schemas",
        "Custom agents" => "tool_prompt",
        "MCP tools" | "MCP server instructions" => "mcp_tool_schemas",
        "Skills" => "skills",
        _ => "messages",
    }
}

/// 卡的组成（kit `stats::Window.breakdown`）：用掉的类别按来源。
pub fn breakdown(usage: &ContextUsage) -> Vec<(String, u64)> {
    usage
        .categories
        .iter()
        .filter(|c| c.kind == CategoryKind::Used)
        .map(|c| (source_of(&c.name).to_owned(), c.tokens))
        .collect()
}

/// 组的键 → 页上的名字。
pub fn group_name(key: &str) -> &'static str {
    match key {
        "system" => "System",
        "tools" => "Tools",
        "MCP" => "MCP",
        "skills" => "Skills",
        _ => "Messages",
    }
}

fn messages(usage: &ContextUsage) -> Node {
    let calls: u64 = usage.tool_results.iter().map(|t| t.calls).sum();
    let kids = usage
        .messages
        .iter()
        .map(|m| {
            if m.kind == "Tool results" && !usage.tool_results.is_empty() {
                Node::leaf(&m.kind, many(calls as usize, "call"), m.tokens).with(
                    usage
                        .tool_results
                        .iter()
                        .map(|t| Node::leaf(&t.tool, many(t.calls as usize, "call"), t.tokens))
                        .collect(),
                )
            } else {
                Node::leaf(&m.kind, "", m.tokens)
            }
        })
        .collect::<Vec<_>>();
    let note = if kids.is_empty() {
        String::new()
    } else {
        many(kids.len(), "kind")
    };
    Node::leaf("Messages", note, 0).with(kids)
}

fn tools(usage: &ContextUsage) -> Node {
    let mut system = Node::leaf(
        "System tools",
        if usage.system_tools.is_empty() {
            String::new()
        } else {
            many(usage.system_tools.len(), "tool")
        },
        category(usage, "System tools"),
    );
    let mut listed: Vec<Node> = usage
        .system_tools
        .iter()
        .map(|t| Node::leaf(&t.name, "", t.tokens))
        .collect();
    if !listed.is_empty() && !usage.deferred_tools.is_empty() {
        let mut more = Node::leaf(
            format!("{} more on demand", usage.deferred_tools.len()),
            "not loaded",
            usage.deferred_tools.iter().map(|t| t.tokens).sum(),
        );
        more.quiet = true;
        listed.push(more);
    }
    system = system.with(listed);
    let agents = Node::leaf(
        "Custom agents",
        many(usage.agents.len(), "agent"),
        category(usage, "Custom agents"),
    )
    .with(
        usage
            .agents
            .iter()
            .map(|a| Node::leaf(&a.agent_type, a.source.to_lowercase(), a.tokens))
            .collect(),
    );
    let mut note = Vec::new();
    if !usage.system_tools.is_empty() {
        note.push(many(usage.system_tools.len(), "tool"));
    }
    if !usage.agents.is_empty() {
        note.push(many(usage.agents.len(), "agent"));
    }
    let kids = [system, agents]
        .into_iter()
        .filter(|n| n.tokens > 0)
        .collect();
    Node::leaf("Tools", note.join(" · "), 0).with(kids)
}

fn mcp(usage: &ContextUsage) -> Node {
    // 已加载的工具按服务器分，服务器按第一次出现的次序。
    let mut servers: Vec<(String, Vec<Node>)> = Vec::new();
    for tool in usage.mcp_tools.iter().filter(|t| !t.deferred) {
        let name = tool
            .name
            .strip_prefix(&format!("mcp__{}__", tool.server))
            .unwrap_or(&tool.name);
        let leaf = Node::leaf(name, "", tool.tokens);
        match servers.iter_mut().find(|(s, _)| *s == tool.server) {
            Some((_, kids)) => kids.push(leaf),
            None => servers.push((tool.server.clone(), vec![leaf])),
        }
    }
    let count = servers.len();
    let mut kids: Vec<Node> = servers
        .into_iter()
        .map(|(server, tools)| {
            let tokens = tools.iter().map(|t| t.tokens).sum();
            Node::leaf(server, many(tools.len(), "tool"), tokens).with(tools)
        })
        .collect();
    let deferred: Vec<_> = usage.mcp_tools.iter().filter(|t| t.deferred).collect();
    if !deferred.is_empty() {
        let mut more = Node::leaf(
            format!("{} more on demand", deferred.len()),
            "not loaded",
            category(usage, "MCP tools (deferred)"),
        );
        more.quiet = true;
        kids.push(more);
    }
    let instructions = category(usage, "MCP server instructions");
    if instructions > 0 {
        kids.push(Node::leaf("Server instructions", "", instructions));
    }
    let mut node = Node::leaf(
        "MCP",
        if count == 0 {
            String::new()
        } else {
            many(count, "server")
        },
        0,
    )
    .with(kids);
    node.pointer = Some("/mcp");
    node
}

fn system(usage: &ContextUsage, places: &Places<'_>) -> Node {
    let prompt = Node::leaf(
        "System prompt",
        if usage.prompt_sections.is_empty() {
            String::new()
        } else {
            many(usage.prompt_sections.len(), "section")
        },
        category(usage, "System prompt"),
    )
    .with(
        usage
            .prompt_sections
            .iter()
            .map(|s| Node::leaf(&s.name, "", s.tokens))
            .collect(),
    );
    let mut memory = Node::leaf(
        "Memory files",
        many(usage.memory_files.len(), "file"),
        category(usage, "Memory files"),
    )
    .with(
        usage
            .memory_files
            .iter()
            .map(|f| Node {
                // 路径里这一段仍是完整路径（刷新后对得上），显示的是短的。
                key: f.path.clone(),
                ..Node::leaf(places.show(&f.path), f.kind.to_lowercase(), f.tokens)
            })
            .collect(),
    );
    memory.pointer = Some("/memory");
    let note = if usage.memory_files.is_empty() {
        "prompt".to_owned()
    } else {
        format!("prompt · {}", many(usage.memory_files.len(), "file"))
    };
    let kids = [prompt, memory]
        .into_iter()
        .filter(|n| n.tokens > 0)
        .collect();
    Node::leaf("System", note, 0).with(kids)
}

fn skills(usage: &ContextUsage) -> Node {
    let mut node = Node::leaf("Skills", many(usage.skills.len(), "skill"), 0).with(
        usage
            .skills
            .iter()
            .map(|s| {
                let source = s.source.to_lowercase();
                let note = match &s.plugin {
                    Some(plugin) => format!("{source} ({plugin})"),
                    None => source,
                };
                Node::leaf(&s.name, note, s.tokens)
            })
            .collect(),
    );
    node.pointer = Some("/skills");
    node
}

/// 大纲的顶层：卡的五个组，次序与档同卡的图例（kit `stats::groups`）；为 0 的组不列。
pub fn outline(usage: &ContextUsage, places: &Places<'_>) -> Vec<Node> {
    stats::groups(&breakdown(usage))
        .into_iter()
        .enumerate()
        .map(|(rank, (key, tokens))| {
            let mut node = match key {
                "system" => system(usage, places),
                "tools" => tools(usage),
                "MCP" => mcp(usage),
                "skills" => skills(usage),
                _ => messages(usage),
            };
            node.key = key.to_owned();
            node.name = group_name(key).to_owned();
            node.tokens = tokens;
            node.rank = Some(rank);
            node
        })
        .collect()
}

/// 看得见的一行：路径、深度、节点与开合（叶子为 None）。
#[derive(Debug, Clone, Copy)]
pub struct Shown<'a> {
    pub depth: usize,
    pub node: &'a Node,
    pub open: Option<bool>,
}

/// 按开合排出看得见的行（与它们的路径）。
pub fn visible<'a>(nodes: &'a [Node], open: &BTreeSet<Path>) -> Vec<(Path, Shown<'a>)> {
    fn walk<'a>(
        node: &'a Node,
        depth: usize,
        path: &mut Path,
        open: &BTreeSet<Path>,
        out: &mut Vec<(Path, Shown<'a>)>,
    ) {
        path.push(node.key.clone());
        let is_open = (!node.kids.is_empty()).then(|| open.contains(path));
        out.push((
            path.clone(),
            Shown {
                depth,
                node,
                open: is_open,
            },
        ));
        if is_open == Some(true) {
            for kid in &node.kids {
                walk(kid, depth + 1, path, open, out);
            }
        }
        path.pop();
    }
    let mut out = Vec::new();
    let mut path = Vec::new();
    for node in nodes {
        walk(node, 0, &mut path, open, &mut out);
    }
    out
}

/// 选中的路径在看得见的行里的位置；不在了时取它还在的最近的上一层，都不在时第一行。
pub fn chosen_at(rows: &[(Path, Shown<'_>)], chosen: &[String]) -> usize {
    (0..=chosen.len())
        .rev()
        .find_map(|n| rows.iter().position(|(p, _)| p.as_slice() == &chosen[..n]))
        .unwrap_or(0)
}

/// 大纲的按键（tui-panels.md §4.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    By(isize),
    First,
    Last,
    /// ⏎：开着的合上，合着的打开。
    Toggle,
    /// →：打开。
    Open,
    /// ←：开着的合上，否则去上一层。
    Close,
}

/// 按一个键之后的选中与开合。
pub fn step(nodes: &[Node], open: &mut BTreeSet<Path>, chosen: &mut Path, how: Move) {
    let rows = visible(nodes, open);
    if rows.is_empty() {
        return;
    }
    let at = chosen_at(&rows, chosen);
    let (path, shown) = &rows[at];
    match how {
        Move::By(by) => {
            let to = (at as isize + by).clamp(0, rows.len() as isize - 1) as usize;
            *chosen = rows[to].0.clone();
        }
        Move::First => *chosen = rows[0].0.clone(),
        Move::Last => *chosen = rows[rows.len() - 1].0.clone(),
        Move::Toggle | Move::Open | Move::Close => {
            *chosen = path.clone();
            match (how, shown.open) {
                (Move::Toggle | Move::Open, Some(false)) => {
                    open.insert(path.clone());
                }
                (Move::Toggle | Move::Close, Some(true)) => {
                    // 合上时下面开着的一起忘掉（再打开时从合着的开始）。
                    open.retain(|p| !p.starts_with(path));
                }
                (Move::Close, _) if path.len() > 1 => {
                    chosen.pop();
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
#[path = "context_tree_tests.rs"]
pub(crate) mod tests;
