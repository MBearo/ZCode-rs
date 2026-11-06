//! 一段工作的调用做了什么（`Tally`），与待办清单的变化（docs/puffin-tui.md §4.2「工作的收拢」「待办」）。
//! 移植自 Puffin 的 shell/tools/tally.rs 的纯函数部分（Tools.scala 第 443–520、655–712 行）；入参是中性的
//! [`Call`]。按条目列表找前一个清单（`todos_before`）留在 Puffin。
use serde_json::Value;

use super::text::{first_line, line_count, patch_files, plural, str_of};
use super::{
    Call, Ink, Part, Status, Steps, Where, failed, input, name, output, target, tool, transparent,
};

/// 一段工作的调用做了什么（§4.2「工作的收拢」）：好好结束的按种类计数；失败的与停下的按名字。
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Tally {
    pub files: i64,
    pub searches: i64,
    pub pages: i64,
    pub web_searches: i64,
    pub edited: Vec<String>,
    pub added: i64,
    pub removed: i64,
    pub commands: i64,
    /// 子代理（`Agent`）：说「跑了」，不说「调用了」（§4.2「工作的收拢」的「句子」）。
    pub subagents: i64,
    /// 那些子代理报告的 token（`subagent_tokens`）；没有报告时为 0。
    pub subagent_tokens: f64,
    /// `Todos` 调用（§4.2「工作的收拢」的待办）：最后一个新清单的步数（没有时 0）、变成完成的步数、更新了几次。
    pub planned: i64,
    pub checked_off: i64,
    pub todo_updates: i64,
    pub others: Vec<String>,
    /// 好好结束的调用的个数。
    pub ok: i64,
    /// 失败的调用的名字。
    pub failed: Vec<String>,
    /// 被停下的调用的名字。
    pub stopped: Vec<String>,
}

fn distinct(v: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in v {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

const COUNTED: [&str; 13] = [
    "Read",
    "Grep",
    "Glob",
    "WebFetch",
    "WebSearch",
    "Edit",
    "Write",
    "NotebookEdit",
    "ApplyPatch",
    "Bash",
    "Agent",
    "Task",
    "TodoWrite",
];

/// `calls` 做了什么。`before` 是 `Todos` 调用（按 id）找到的清单。
pub fn tally(calls: &[Call<'_>], where_: &Where, before: &dyn Fn(&str) -> Option<Steps>) -> Tally {
    let ended: Vec<&Call<'_>> = calls
        .iter()
        .filter(|c| c.ended && !transparent(c))
        .collect();
    let failures: Vec<&Call<'_>> = ended.iter().copied().filter(|c| failed(c)).collect();
    let stops: Vec<&Call<'_>> = ended
        .iter()
        .copied()
        .filter(|c| !failed(c) && c.status == Status::Cancelled)
        .collect();
    let good: Vec<&Call<'_>> = ended
        .iter()
        .copied()
        .filter(|c| !failed(c) && c.status != Status::Cancelled)
        .collect();
    let of = |names: &[&str]| -> Vec<&Call<'_>> {
        good.iter()
            .copied()
            .filter(|c| names.contains(&tool(c)))
            .collect()
    };
    let no_task = |_: &str| None::<String>;
    // 改动：每个文件一次，加减是整段的和；新建的文件的行数算加。
    let mut changes: Vec<(String, i64, i64)> = Vec::new();
    for c in &good {
        let i = input(c);
        match tool(c) {
            "Edit" | "Write" | "NotebookEdit" => {
                let path = target(c, where_, &no_task, &super::no_todos);
                changes.push(match c.facts.and_then(|f| f.diff) {
                    Some(d) => (path, d.added as i64, d.removed as i64),
                    None if output(c).starts_with("File created successfully") => {
                        (path, line_count(str_of(i, "content")), 0)
                    }
                    None => (path, 0, 0),
                });
            }
            "ApplyPatch" => changes.extend(patch_files(str_of(i, "patch_text"), where_)),
            _ => {}
        }
    }
    let lists: Vec<TodoChange> = of(&["TodoWrite"])
        .into_iter()
        .map(|c| todo_change(c, before(c.id).as_ref()))
        .collect();
    let agents = of(&["Agent", "Task"]);
    Tally {
        files: distinct(
            of(&["Read"])
                .iter()
                .map(|c| str_of(input(c), "file_path").to_owned())
                .collect(),
        )
        .len() as i64,
        searches: of(&["Grep", "Glob"]).len() as i64,
        pages: of(&["WebFetch"]).len() as i64,
        web_searches: of(&["WebSearch"]).len() as i64,
        edited: distinct(changes.iter().map(|c| c.0.clone()).collect()),
        added: changes.iter().map(|c| c.1).sum(),
        removed: changes.iter().map(|c| c.2).sum(),
        commands: of(&["Bash"]).len() as i64,
        subagents: agents.len() as i64,
        subagent_tokens: agents
            .iter()
            .filter_map(|c| super::reported(c, "subagent_tokens"))
            .sum(),
        planned: lists
            .iter()
            .rfind(|l| l.new_list)
            .map_or(0, |l| l.steps.len() as i64),
        checked_off: lists.iter().map(|l| l.finished.len() as i64).sum(),
        todo_updates: lists.len() as i64,
        others: good
            .iter()
            .filter(|c| !COUNTED.contains(&tool(c)))
            .map(|c| name(c))
            .collect(),
        ok: good.len() as i64,
        failed: failures.iter().map(|c| name(c)).collect(),
        stopped: stops.iter().map(|c| name(c)).collect(),
    }
}

// ---- 待办清单（§4.2「待办」）----

/// 输入里的待办：每一步的状态与文字。
pub fn todos(i: &Value) -> Steps {
    match i.get("todos").and_then(Value::as_array) {
        None => Vec::new(),
        Some(list) => list
            .iter()
            .map(|t| {
                (
                    str_of(t, "status").to_owned(),
                    first_line(str_of(t, "content")),
                )
            })
            .collect(),
    }
}

/// `Todos` 调用对它找到的清单做了什么。新清单是第一个，或步骤的文字不同的；步骤按文字区分。
#[derive(Clone, Debug, PartialEq, Default)]
pub struct TodoChange {
    pub steps: Steps,
    pub new_list: bool,
    pub finished: Vec<String>,
    pub started: Vec<String>,
    pub reopened: Vec<String>,
}

impl TodoChange {
    pub fn done(&self) -> usize {
        self.steps.iter().filter(|s| s.0 == "completed").count()
    }

    pub fn all_done(&self) -> bool {
        !self.steps.is_empty() && self.done() == self.steps.len()
    }
}

pub fn todo_change(c: &Call<'_>, before: Option<&Steps>) -> TodoChange {
    let steps = todos(input(c));
    // Scala 的 `toMap`：同一文字后来的状态覆盖先前的。
    let was = |t: &str| -> Option<&str> {
        before.and_then(|b| b.iter().rev().find(|s| s.1 == t).map(|s| s.0.as_str()))
    };
    let texts = |s: &Steps| s.iter().map(|x| x.1.clone()).collect::<Vec<_>>();
    let new_list = before.is_none_or(|b| texts(b) != texts(&steps));
    let pick = |f: &dyn Fn(&str, &str) -> bool| -> Vec<String> {
        steps
            .iter()
            .filter(|(st, t)| f(st, t))
            .map(|(_, t)| t.clone())
            .collect()
    };
    TodoChange {
        finished: pick(&|st, t| st == "completed" && was(t) != Some("completed")),
        started: pick(&|st, t| st == "in_progress" && was(t) != Some("in_progress")),
        reopened: pick(&|st, t| st != "completed" && was(t) == Some("completed")),
        new_list,
        steps,
    }
}

/// `Todos` 调用的对象：新清单是 `6 steps`，否则是变了什么：`finished A · started B`、`finished A · all done`、`no changes`。
pub fn todo_words(c: &TodoChange) -> Vec<Part> {
    if c.new_list {
        return vec![Part::new(plural(c.steps.len() as i64, "step"), Ink::Fg3)];
    }
    let said = |verb: &str, ts: &[String]| -> Option<Vec<Part>> {
        (!ts.is_empty()).then(|| {
            let what = if ts.len() <= 2 {
                ts.join(" and ")
            } else {
                plural(ts.len() as i64, "step")
            };
            vec![
                Part::new(verb, Ink::Fg2),
                Part::new(format!(" {what}"), Ink::Fg3),
            ]
        })
    };
    let mut clauses: Vec<Vec<Part>> = [
        said("finished", &c.finished),
        said("started", &c.started),
        said("reopened", &c.reopened),
    ]
    .into_iter()
    .flatten()
    .collect();
    if c.all_done() {
        clauses.push(vec![Part::new("all done", Ink::Fg3)]);
    }
    if clauses.is_empty() {
        return vec![Part::new("no changes", Ink::Fg3)];
    }
    let mut out = Vec::new();
    for (k, p) in clauses.into_iter().enumerate() {
        if k > 0 {
            out.push(Part::new(" · ", Ink::Line));
        }
        out.extend(p);
    }
    out
}
