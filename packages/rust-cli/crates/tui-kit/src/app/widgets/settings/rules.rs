//! 设置的 Permissions 段的措辞（docs/specs/rust-cli/tui-manage.md §1.2）：三组的标题、来源列与种类、规则的说法，与选中
//! 的规则、规则页、删之前与删之后说的话。入参是中性的 [`Rule`]（工具、内容与写回的规则串）、[`Behavior`] 与 [`Source`]。
//! 移植自 Puffin 的 shell/settings/permissions.rs 的措辞部分；规则串的解析与写回（kernel-api 的
//! `parse_rule` / `format_rule`）、段与页的行和它们的动作留在壳。
use super::{Ink, Part};

/// 规则的种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Behavior {
    Allow,
    Ask,
    Deny,
}

/// 规则从哪里来。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    UserSettings,
    ProjectSettings,
    LocalSettings,
    CliArg,
    Session,
}

/// 一条规则：工具、内容（没有时 `None`），与写回的规则串（`Bash(cargo test:*)`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule<'a> {
    pub tool: &'a str,
    pub content: Option<&'a str>,
    pub text: &'a str,
}

/// 三组：标题与它的种类，按这个次序。
pub const GROUPS: [(Behavior, &str); 3] = [
    (Behavior::Allow, "Runs without asking"),
    (Behavior::Ask, "Always asks"),
    (Behavior::Deny, "Never runs"),
];

/// 规则什么时候生效：内核换上新的规则快照，下一次工具调用起。
pub const EFFECT: &str = "from its next tool call";
pub const EMPTY: &str =
    "Nothing here yet. Pick Always allow on a request and its rule shows up here.";
pub const ADD: &str = "Allow, ask or refuse a command, a file or a site for this project.";
pub const NEW_DESC: &str = "A rule for this project, kept on this computer like the ones Always allow adds. Type a command, a web address, or a rule like Edit(src/**).";
pub const RULE_HINT: &str = "a command, a web address, or Tool(specifier)";
pub const NEEDS_RULE: &str = "Needs a rule.";

pub fn behavior_value(b: Behavior) -> &'static str {
    match b {
        Behavior::Allow => "allow",
        Behavior::Ask => "ask",
        Behavior::Deny => "deny",
    }
}

pub fn behavior_of(value: &str) -> Option<Behavior> {
    GROUPS
        .iter()
        .map(|(b, _)| *b)
        .find(|b| behavior_value(*b) == value)
}

pub fn behavior_name(b: Behavior) -> &'static str {
    GROUPS
        .iter()
        .find(|(x, _)| *x == b)
        .map_or("", |(_, name)| name)
}

pub fn source_value(s: Source) -> &'static str {
    match s {
        Source::UserSettings => "user",
        Source::ProjectSettings => "project",
        Source::LocalSettings => "local",
        Source::CliArg => "cli",
        Source::Session => "session",
    }
}

/// 来源列的词。
pub fn source_word(s: Source) -> &'static str {
    match s {
        Source::UserSettings => "user config",
        Source::ProjectSettings => "project config",
        Source::LocalSettings => "this project",
        Source::CliArg => "command line",
        Source::Session => "this session",
    }
}

/// 配置文件来源的规则只读（内核拒绝删它们）。
pub fn from_config(s: Source) -> bool {
    matches!(s, Source::UserSettings | Source::ProjectSettings)
}

/// `mcp__服务器__工具` 的服务器与工具（`*` 与没有都是整个服务器）。
fn mcp_parts(tool: &str) -> Option<(&str, Option<&str>)> {
    let rest = tool.strip_prefix("mcp__")?;
    Some(match rest.split_once("__") {
        Some((server, "*")) => (server, None),
        Some((server, tool)) => (server, Some(tool)),
        None => (rest, None),
    })
}

/// Bash 规则的前缀：`X:*` 或 `X *` 的 X。
pub fn prefix(content: &str) -> Option<&str> {
    content
        .strip_suffix(":*")
        .or_else(|| content.strip_suffix(" *"))
}

/// WebFetch 规则的主机：`domain:` 之后的。
fn host(content: &str) -> &str {
    content.strip_prefix("domain:").unwrap_or(content)
}

/// 规则的说法，与请求的 Always allow 同一套：命令是前缀加 ` …`，Edit 是 `edits to …`。
pub fn phrase(rule: Rule<'_>) -> String {
    match (rule.tool, rule.content) {
        ("Bash", None) => "any command".to_owned(),
        ("Bash", Some(c)) => prefix(c).map_or_else(|| c.to_owned(), |p| format!("{p} …")),
        ("Edit", Some(c)) => format!("edits to {c}"),
        ("Edit", None) => "any edit".to_owned(),
        ("Read", Some(c)) => format!("reads of {c}"),
        ("Read", None) => "any read".to_owned(),
        ("WebFetch", Some(c)) => format!("pages on {}", host(c)),
        ("WebFetch", None) => "any web page".to_owned(),
        ("WebSearch", _) => "any web search".to_owned(),
        (tool, _) => match mcp_parts(tool) {
            Some((server, Some(t))) => format!("{server} · {t}"),
            Some((server, None)) => server.to_owned(),
            None => rule.text.to_owned(),
        },
    }
}

/// 说法去掉前缀的 ` …`：确认与删掉之后说的。
pub fn short(rule: Rule<'_>) -> String {
    let p = phrase(rule);
    p.strip_suffix(" …")
        .map_or_else(|| p.clone(), str::to_owned)
}

/// 来源列之后的种类。
pub fn kind_word(tool: &str) -> &'static str {
    match tool {
        "Bash" => "command",
        "Edit" => "edit",
        "Read" => "read",
        "WebFetch" => "web page",
        "WebSearch" => "web search",
        t if t.starts_with("mcp__") => "MCP tool",
        _ => "tool",
    }
}

/// 名词：选中的规则下面那句话里的。
pub fn noun(rule: Rule<'_>) -> String {
    match (rule.tool, rule.content) {
        ("Bash", Some(c)) => prefix(c).map_or_else(
            || format!("the command {c}"),
            |p| format!("any command that starts with {p}"),
        ),
        _ => phrase(rule),
    }
}

/// 带墨的名词（添加页的 Means）：具体的部分 fg，其余 fg3。
pub fn means(rule: Rule<'_>) -> Vec<Part> {
    let fg3 = |t: &str| Part::new(t, Ink::Fg3);
    let fg = |t: &str| Part::new(t, Ink::Fg);
    match (rule.tool, rule.content) {
        ("Bash", Some(c)) => match prefix(c) {
            Some(p) => vec![fg3("commands that start with "), fg(p)],
            None => vec![fg3("the command "), fg(c)],
        },
        ("Edit", Some(c)) => vec![fg3("edits to "), fg(c)],
        ("Read", Some(c)) => vec![fg3("reads of "), fg(c)],
        ("WebFetch", Some(c)) => vec![fg3("pages on "), fg(host(c)), fg3(", any path")],
        (tool, _) => match mcp_parts(tool) {
            Some((server, Some(t))) => vec![fg3("the tool "), fg(t), fg3(" of "), fg(server)],
            Some((server, None)) => vec![fg3("any tool of "), fg(server)],
            None if rule.content.is_none() && kind_word(tool) == "tool" => {
                vec![fg3("any use of "), fg(tool)]
            }
            None => vec![fg3(&phrase(rule))],
        },
    }
}

/// 动词短语：规则页的说明（`run any command that starts with cargo test`）。
pub fn verb(rule: Rule<'_>) -> String {
    match (rule.tool, rule.content) {
        ("Bash", None) => "run any command".to_owned(),
        ("Bash", Some(c)) => prefix(c).map_or_else(
            || format!("run {c}"),
            |p| format!("run any command that starts with {p}"),
        ),
        ("Edit", Some(c)) => format!("edit {c}"),
        ("Edit", None) => "edit any file".to_owned(),
        ("Read", Some(c)) => format!("read {c}"),
        ("Read", None) => "read any file".to_owned(),
        ("WebFetch", Some(c)) => format!("fetch pages on {}", host(c)),
        ("WebFetch", None) => "fetch any web page".to_owned(),
        ("WebSearch", _) => "search the web".to_owned(),
        (tool, _) if tool.starts_with("mcp__") => format!("call {}", phrase(rule)),
        _ => format!("use {}", rule.text),
    }
}

/// 动词短语的动名词（`running any command …`）。
pub fn gerund(rule: Rule<'_>) -> String {
    let v = verb(rule);
    let (head, rest) = v.split_once(' ').unwrap_or((v.as_str(), ""));
    let head = match head {
        "run" => "running",
        "edit" => "editing",
        "read" => "reading",
        "fetch" => "fetching",
        "search" => "searching",
        "call" => "calling",
        _ => "using",
    };
    format!("{head} {rest}")
}

/// 选中的规则下面说它做什么。
pub fn sentence(behavior: Behavior, rule: Rule<'_>) -> String {
    let what = noun(rule);
    match behavior {
        Behavior::Allow => format!("Allows {what}, without asking."),
        Behavior::Ask => format!("Asks before {what}."),
        Behavior::Deny => format!("Refuses {what}."),
    }
}

/// 规则页的说明。
pub fn page_desc(behavior: Behavior, rule: Rule<'_>) -> String {
    match behavior {
        Behavior::Allow => format!("Lets the agent {}, without asking.", verb(rule)),
        Behavior::Ask => format!(
            "Makes the agent ask before it {}, whatever the mode.",
            verb(rule)
        ),
        Behavior::Deny => format!(
            "Stops the agent from {}; this wins over the other rules.",
            gerund(rule)
        ),
    }
}

/// 删之前问的那句。
pub fn removal_note(behavior: Behavior, rule: Rule<'_>) -> String {
    match behavior {
        Behavior::Allow => format!("The agent will ask before {} again.", short(rule)),
        _ => format!("Your mode decides about {} again.", short(rule)),
    }
}

/// 删掉之后输入框上方说的。
pub fn removed_said(behavior: Behavior, rule: Rule<'_>) -> String {
    match behavior {
        Behavior::Allow => format!(
            "Removed. The agent asks before {} again, {EFFECT}.",
            short(rule)
        ),
        _ => format!(
            "Removed. Your mode decides about {} again, {EFFECT}.",
            short(rule)
        ),
    }
}

/// 改过种类或规则串之后输入框上方说的。
pub fn replaced_said() -> String {
    format!("Saved. The agent uses it {EFFECT}.")
}

/// 添加之后输入框上方说的。
pub fn added_said() -> String {
    format!("Added. The agent uses it {EFFECT}.")
}

/// 种类的选项（规则页与添加页的 Kind）。
pub fn kind_options() -> Vec<super::Opt> {
    use super::Opt;
    vec![
        Opt::new("allow", "Runs without asking").desc("no question first"),
        Opt::new("ask", "Always asks").desc("a question every time, whatever the mode"),
        Opt::new("deny", "Never runs").desc("refused; wins over the other rules"),
    ]
}

/// 配置文件的规则选中时说它从哪里来（`file` 是写好的路径）。
pub fn config_desc(file: Option<&str>) -> String {
    match file {
        Some(f) => format!("From {f}, which ZCode only reads."),
        None => "From a config file, which ZCode only reads.".to_owned(),
    }
}

/// 配置文件的规则上 ⏎ 的说法。
pub const OPEN_FILE: &str = "open the file";

/// 规则页的 Kept：本项目的规则存在哪里（`place` 是写好的工作区根，没有时 `None`）。
pub fn kept_local(place: Option<&str>) -> String {
    format!("for {}, on this computer", place.unwrap_or("this project"))
}

/// 规则页的 Kept：命令行与会话的规则。
pub fn kept_other(source: Source) -> &'static str {
    if source == Source::CliArg {
        "for this run, from the command line"
    } else {
        "for this session"
    }
}

/// 工作的文件夹的标题。
pub const FOLDERS: &str = "Folders it works in";

/// 一个工作的文件夹的来源列与说明：`added` 是 `--add-dir` 加的。
pub fn folder_words(added: bool) -> (&'static str, &'static str) {
    if added {
        ("--add-dir", "Added with --add-dir when ZCode started.")
    } else {
        ("working folder", "The agent works here.")
    }
}
