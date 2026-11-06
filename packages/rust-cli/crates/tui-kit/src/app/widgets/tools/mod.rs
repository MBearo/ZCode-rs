//! 工具调用的行说什么（docs/puffin-tui.md §4.2「工具调用」；docs/specs/rust-cli/tui-kit.md §4.4）：名字表、对象、结果、
//! 挂在下面的东西与一段工作数出的 `Tally`。纯函数，入参是中性的 [`Call`]（一次调用的事实，全是借来的字与数），两套界面
//! 各自把自己的条目映射进来；[`super::tool_row`] 画它。
//! 移植自 Puffin 的 shell/tools.rs 的纯函数部分（Puffin 的条目 → [`Call`] 的映射与按条目列表的分组留在
//! Puffin）。文字工具在 `text.rs`，结果在 `outcome.rs`，`Tally` 与待办在 `tally.rs`。
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::fit;

pub mod outcome;
pub mod tally;
pub mod text;

pub use outcome::{live, outcome, under};
pub use tally::{Tally, TodoChange, tally, todo_change, todo_words, todos};
use text::{
    exit_line, find_figure, first_line, first_string, fmt, jtrim, num_of, patch_files, plural,
    relative, short, str_of, uncd,
};

/// 调用的状态（投影的 `ToolStatus` 的中性写法）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Running,
    Queued,
    Asking,
    Done,
    Error,
    Cancelled,
}

/// MCP 工具的服务器与工具名。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mcp<'a> {
    pub server: &'a str,
    pub tool: &'a str,
}

/// MCP 调用的进度：`fraction` 未知时为负。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Progress<'a> {
    pub fraction: f64,
    pub message: &'a str,
}

/// 差异的一行：`line` 是删掉的行在旧文本、其余在新文本里的行号（从改动的片段算起）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NumberedLine {
    pub sign: Sign,
    pub text: String,
    pub line: u64,
}

/// 差异行的符号：`+`、`-`、` `（上下文）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sign {
    #[serde(rename = "+")]
    Add,
    #[serde(rename = "-")]
    Remove,
    #[serde(rename = " ")]
    Context,
}

/// 编辑的改动：加减的行数与差异的行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Changes<'a> {
    pub added: u64,
    pub removed: u64,
    pub lines: &'a [NumberedLine],
}

/// 在跑的子代理此刻的调用：工具名与输入。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Current<'a> {
    pub tool: &'a str,
    pub input: &'a Value,
}

/// 整理好的事实（Puffin 投影的 `ToolFacts`）：`tool` 是内核的工具名原样（MCP 是 `mcp__服务__工具`），不为空。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Facts<'a> {
    pub tool: &'a str,
    /// 模型给的输入原样。
    pub input: &'a Value,
    /// 结果的文字；没有时为 ""。
    pub output: &'a str,
    /// 出错时的文字；没有时为 ""。
    pub error: &'a str,
    pub diff: Option<Changes<'a>>,
    /// 运行中的命令的实时输出（最后几行）。
    pub preview: &'a str,
    /// 输入还在写出。
    pub writing: bool,
    /// 毫秒；没有时为 0。
    pub started_at: u64,
    pub ended_at: u64,
    pub progress: Option<Progress<'a>>,
    pub backgrounded: bool,
    pub mcp: Option<Mcp<'a>>,
    /// 读后台任务输出的调用说的任务状态；没有时为 ""。
    pub task_status: &'a str,
    /// 停下后台任务的调用的命令；没有时为 ""。
    pub command: &'a str,
    /// 在跑的子代理此刻的调用。
    pub current: Option<Current<'a>>,
}

static NO_INPUT: Value = Value::Null;

impl<'a> Facts<'a> {
    /// 只有名字与输入的事实（子代理此刻的调用）。
    pub fn named(tool: &'a str, input: &'a Value) -> Facts<'a> {
        Facts {
            tool,
            input,
            output: "",
            error: "",
            diff: None,
            preview: "",
            writing: false,
            started_at: 0,
            ended_at: 0,
            progress: None,
            backgrounded: false,
            mcp: mcp_of_name(tool),
            task_status: "",
            command: "",
            current: None,
        }
    }
}

/// 一次工具调用，作为中性的数据：条目 id、投影给的名字与摘要（没有事实时用）、状态、所在的模型回答、是否结束，与事实。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Call<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub summary: &'a str,
    pub status: Status,
    /// 调用所在的模型回答（§4.2「一批调用」）；不知道时为 ""。
    pub response: &'a str,
    pub ended: bool,
    /// 没有事实的调用（还在排队的）为 `None`。
    pub facts: Option<Facts<'a>>,
}

/// 行里一段的墨色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ink {
    Fg2,
    Fg3,
    Ok,
    Err,
    Line,
}

/// 行里的一段文字与它的墨色。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub text: String,
    pub ink: Ink,
}

impl Part {
    pub fn new(text: impl Into<String>, ink: Ink) -> Part {
        Part {
            text: text.into(),
            ink,
        }
    }
}

/// 路径怎样写短：cwd 之下相对，家目录下写成 `~`。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Where {
    pub cwd: String,
    pub home: Option<String>,
}

/// 哪里都不是：路径照原样。
pub fn nowhere() -> Where {
    Where::default()
}

/// 待办清单的步骤：（状态，文字）。
pub type Steps = Vec<(String, String)>;

/// 一行 diff：行号、`+` `-` ` ` 与文字。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffRow {
    pub n: i64,
    pub sign: char,
    pub text: String,
}

/// 挂在行下面的东西。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Under {
    Nothing,
    /// diff 的行。
    Diff(Vec<DiffRow>),
    /// 补丁：每个文件与它加减的行数。
    Files(Vec<(String, i64, i64)>),
    /// 命令的最后几行。
    Tail(Vec<String>),
    /// 展开的命令：命令、模型为什么跑它、它的全部输出。
    Shell {
        command: String,
        why: String,
        lines: Vec<String>,
    },
    /// 调用为什么失败。
    Reason(Vec<String>),
    /// 调用交回的：子代理的报告、MCP 工具的文字。
    Report(Vec<String>),
    /// 在跑的子代理此刻的调用。
    Ticker(String),
    /// 新的待办清单：每一步的状态与文字（§4.2「待办」）。
    Todos(Steps),
}

/// 收拢的一段的行下面显示的在跑的命令的输出行数（§4.2「工作的收拢」）。
pub const TAIL_ROWS: usize = 3;

const EXPLORE: [&str; 5] = ["Read", "Grep", "Glob", "WebFetch", "WebSearch"];
const HIDDEN: [&str; 3] = ["ToolSearch", "TodoRead", "GoalRead"];
const PLAIN: [(&str, &str); 16] = [
    ("Grep", "Search"),
    ("Glob", "Find"),
    ("WebFetch", "Fetch"),
    ("WebSearch", "Web search"),
    ("TodoWrite", "Todos"),
    ("ApplyPatch", "Patch"),
    ("js", "JavaScript"),
    ("TaskOutput", "Output"),
    ("BashOutput", "Output"),
    ("AgentOutput", "Output"),
    ("TaskStop", "Stop"),
    ("KillShell", "Stop"),
    ("KillBash", "Stop"),
    ("CronCreate", "Schedule"),
    ("AskUserQuestion", "Question"),
    ("McpElicitation", "Question"),
];
const PLAN: (&str, &str) = ("ExitPlanMode", "Plan");
/// 读后台任务输出的工具。
pub const OUTPUTS: [&str; 3] = ["TaskOutput", "BashOutput", "AgentOutput"];
const STOPS: [&str; 3] = ["TaskStop", "KillShell", "KillBash"];
const MCP_ERROR_HEAD: &str = "MCP tool returned an error:";

// ---- 事实 ----

/// agent 给工具的名字；没有事实的调用用投影给的名字。
pub fn tool<'a>(c: &Call<'a>) -> &'a str {
    match &c.facts {
        Some(f) => f.tool,
        None => c.name,
    }
}

/// 调用在哪台远端机器上（docs/specs/rust-cli/remote-exec.md §21.6）：输入里的 `machine`（不是 `local`），否则 MCP
/// 服务器绑定的机器（`mcp_machines`：服务器 → 机器）；在本机时为 `None`。
pub fn machine<'a>(c: &Call<'a>, mcp_machines: &'a [(String, String)]) -> Option<&'a str> {
    let named = input(c)["machine"]
        .as_str()
        .map(str::trim)
        .filter(|m| !m.is_empty() && *m != "local");
    named.or_else(|| {
        let server = mcp(c)?.server;
        mcp_machines
            .iter()
            .find(|(s, _)| s == server)
            .map(|(_, m)| m.as_str())
    })
}

pub fn input<'a>(c: &Call<'a>) -> &'a Value {
    c.facts.map_or(&NO_INPUT, |f| f.input)
}

pub fn output<'a>(c: &Call<'a>) -> &'a str {
    c.facts.map_or("", |f| f.output)
}

pub fn mcp<'a>(c: &Call<'a>) -> Option<Mcp<'a>> {
    c.facts.and_then(|f| f.mcp)
}

/// 调用的输入还在写出（§4.2「笔」的「调用」）；输入完整、或不知道时为假。
pub fn writing(c: &Call<'_>) -> bool {
    !c.ended && status(c) == "running" && c.facts.is_some_and(|f| f.writing)
}

/// 子代理的报告结尾的数字（`subagent_tokens`、`duration_ms`、`tool_uses`）；没有时为 `None`。
pub fn reported(c: &Call<'_>, key: &str) -> Option<f64> {
    find_figure(output(c), key)
}

/// 调用开始与结束的时刻（毫秒）；不知道时为 0。
pub fn times(c: &Call<'_>) -> (f64, f64) {
    c.facts
        .map_or((0.0, 0.0), |f| (f.started_at as f64, f.ended_at as f64))
}

/// 画成子代理的（§4.2「子代理的数字」）：没有送到后台的 `Agent` 调用。
pub fn subagent(c: &Call<'_>) -> bool {
    matches!(tool(c), "Agent" | "Task") && !background(c)
}

// ---- 哪些调用 ----

/// 读、搜索与抓取：一段连着的折成一行。
pub fn explores(tool: &str) -> bool {
    EXPLORE.contains(&tool)
}

/// 你没有要的管道：不画，除非失败了或 ^O 开着。
pub fn hidden(tool: &str) -> bool {
    HIDDEN.contains(&tool)
}

/// 既不显示、也不打断一段的管道：隐藏的、没有失败的调用。
pub fn transparent(c: &Call<'_>) -> bool {
    hidden(tool(c)) && !failed(c)
}

/// 读、搜索与抓取的调用。
pub fn reads(c: &Call<'_>) -> bool {
    explores(tool(c))
}

// ---- 行 ----

/// 行显示的名字。
pub fn name(c: &Call<'_>) -> String {
    if let Some(m) = mcp(c) {
        return m.tool.to_owned();
    }
    if c.facts.is_none() {
        return c.name.to_owned();
    }
    let t = tool(c);
    PLAIN
        .iter()
        .chain(std::iter::once(&PLAN))
        .find(|(k, _)| *k == t)
        .map_or_else(|| t.to_owned(), |(_, v)| (*v).to_owned())
}

/// 没有清单：每个 `Todos` 调用都是新清单。
pub fn no_todos(_: &str) -> Option<Steps> {
    None
}

/// 它作用于什么，一行。`tasks` 找后台任务跑的命令；`before` 是 `Todos` 调用（按 id）找到的清单。
pub fn target(
    c: &Call<'_>,
    where_: &Where,
    tasks: &dyn Fn(&str) -> Option<String>,
    before: &dyn Fn(&str) -> Option<Steps>,
) -> String {
    let Some(f) = c.facts else {
        return first_line(c.summary);
    };
    let i = f.input;
    let path = |key: &str| short(str_of(i, key), where_);
    let text = match f.mcp {
        Some(m) => std::iter::once(m.server.to_owned())
            .chain(first_string(i))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · "),
        None => match f.tool {
            "Read" | "Edit" | "Write" | "NotebookEdit" => path("file_path"),
            "Grep" => {
                let p = path("path");
                let within = if p.is_empty() {
                    String::new()
                } else {
                    format!(" in {p}")
                };
                format!("{}{within}", str_of(i, "pattern"))
            }
            "Glob" => str_of(i, "pattern").to_owned(),
            "WebFetch" => {
                let url = str_of(i, "url");
                url.strip_prefix("https://")
                    .or_else(|| url.strip_prefix("http://"))
                    .unwrap_or(url)
                    .to_owned()
            }
            "WebSearch" => str_of(i, "query").to_owned(),
            "ApplyPatch" => {
                let n = patch_files(str_of(i, "patch_text"), where_).len();
                if n == 0 {
                    String::new()
                } else {
                    plural(n as i64, "file")
                }
            }
            "Bash" => relative(&uncd(str_of(i, "command"), &where_.cwd), &where_.cwd),
            o if OUTPUTS.contains(&o) || STOPS.contains(&o) => {
                let id = match str_of(i, "task_id") {
                    "" => str_of(i, "shell_id"),
                    s => s,
                };
                let said = tasks(id)
                    .or_else(|| (!f.command.is_empty()).then(|| f.command.to_owned()))
                    .unwrap_or_else(|| id.to_owned());
                relative(&said, &where_.cwd)
            }
            "Agent" | "Task" => str_of(i, "description").to_owned(),
            "Skill" => match str_of(i, "skill") {
                "" => str_of(i, "name").to_owned(),
                s => s.to_owned(),
            },
            "TodoWrite" => todo_words(&todo_change(c, before(c.id).as_ref()))
                .into_iter()
                .map(|p| p.text)
                .collect(),
            "CronCreate" => {
                let when = match str_of(i, "cron") {
                    "" => num_of(i, "delayMinutes")
                        .map_or_else(String::new, |m| format!("in {} min", fmt(m))),
                    cr => cr.to_owned(),
                };
                let what = match str_of(i, "title") {
                    "" => str_of(i, "prompt"),
                    s => s,
                };
                [when.as_str(), what]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · ")
            }
            "js" => str_of(i, "title").to_owned(),
            _ => c.summary.to_owned(),
        },
    };
    first_line(&text)
}

/// `Name target`：跑马灯里一个调用的标签。
pub fn label(
    c: &Call<'_>,
    where_: &Where,
    tasks: &dyn Fn(&str) -> Option<String>,
    before: &dyn Fn(&str) -> Option<Steps>,
) -> String {
    let t = target(c, where_, tasks, before);
    if t.is_empty() {
        name(c)
    } else {
        format!("{} {t}", name(c))
    }
}

/// 子代理此刻的调用，作为一个在跑的调用。
pub fn as_call<'a>(cur: Current<'a>) -> Call<'a> {
    Call {
        id: "",
        name: cur.tool,
        summary: "",
        status: Status::Running,
        response: "",
        ended: false,
        facts: (!cur.tool.is_empty()).then(|| Facts::named(cur.tool, cur.input)),
    }
}

/// 子代理此刻的调用，写得像一行的标签。
pub fn call_label(cur: Current<'_>, where_: &Where) -> String {
    label(&as_call(cur), where_, &|_| None, &no_todos)
}

/// 子代理此刻的调用：名字与对象。
pub fn call_parts(cur: Current<'_>, where_: &Where) -> (String, String) {
    let c = as_call(cur);
    (name(&c), target(&c, where_, &|_| None, &no_todos))
}

/// 在跑的子代理此刻的调用。
pub fn current<'a>(c: &Call<'a>) -> Option<Current<'a>> {
    c.facts.and_then(|f| f.current)
}

/// `mcp__服务__工具` 的服务与工具；别的名字为 `None`。
pub fn mcp_of_name(tool: &str) -> Option<Mcp<'_>> {
    let rest = tool.strip_prefix("mcp__")?;
    let k = rest.find("__").filter(|k| *k > 0)?;
    Some(Mcp {
        server: &rest[..k],
        tool: &rest[k + 2..],
    })
}

/// 它失败了：状态这样说，或命令以非零退出、超时，或 MCP 工具交回错误。
pub fn failed(c: &Call<'_>) -> bool {
    c.status == Status::Error
        || (c.status == Status::Done
            && (exit_code(c).is_some() || timed_out(c) || mcp_error(c).is_some()))
}

/// 记号显示的状态：`running`、`queued`、`asking`、`done`、`error` 或 `cancelled`。
pub fn status(c: &Call<'_>) -> &'static str {
    if failed(c) {
        return "error";
    }
    match c.status {
        Status::Queued => "queued",
        Status::Asking => "asking",
        Status::Done => "done",
        Status::Error => "error",
        Status::Cancelled => "cancelled",
        Status::Running => "running",
    }
}

/// 命令的非零退出码。
pub fn exit_code(c: &Call<'_>) -> Option<i64> {
    if tool(c) != "Bash" {
        return None;
    }
    exit_line(output(c)).filter(|n| *n != 0)
}

pub fn timed_out(c: &Call<'_>) -> bool {
    tool(c) == "Bash" && output(c).contains("Command timed out after ")
}

/// 送到后台的命令或子代理。
pub fn background(c: &Call<'_>) -> bool {
    c.facts.is_some_and(|f| f.backgrounded)
        || output(c).starts_with("Command running in background")
        || output(c).starts_with("Async agent launched")
}

/// MCP 工具交回的错误。
pub fn mcp_error(c: &Call<'_>) -> Option<String> {
    (mcp(c).is_some() && output(c).starts_with(MCP_ERROR_HEAD))
        .then(|| jtrim(&output(c)[MCP_ERROR_HEAD.len()..]).to_owned())
}

/// 一段读说成话：`3 files, 1 search`；读了两次的文件算一次。
pub fn counts(members: &[Call<'_>]) -> String {
    let mut files: Vec<&str> = Vec::new();
    for m in members.iter().filter(|m| tool(m) == "Read") {
        let p = str_of(input(m), "file_path");
        if !files.contains(&p) {
            files.push(p);
        }
    }
    let count = |names: &[&str]| members.iter().filter(|m| names.contains(&tool(m))).count();
    let searches = count(&["Grep", "Glob"]);
    let pages = count(&["WebFetch"]);
    let web = count(&["WebSearch"]);
    let parts = [
        (!files.is_empty()).then(|| plural(files.len() as i64, "file")),
        (searches > 0).then(|| {
            if searches == 1 {
                "1 search".to_owned()
            } else {
                format!("{searches} searches")
            }
        }),
        (pages > 0).then(|| plural(pages as i64, "page")),
        (web > 0).then(|| {
            if web == 1 {
                "1 web search".to_owned()
            } else {
                format!("{web} web searches")
            }
        }),
    ];
    parts.into_iter().flatten().collect::<Vec<_>>().join(", ")
}

/// 调用用了多久（同 [`fit::time`]）。
pub fn time(ms: f64) -> String {
    fit::time(ms)
}
