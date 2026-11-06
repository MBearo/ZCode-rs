//! 调用的结果、进行中的末尾与挂在下面的东西（docs/puffin-tui.md §4.2「工具调用」的「结果」「证据」）。
//! 移植自 Puffin 的 shell/tools/outcome.rs 的纯函数部分（Tools.scala 第 300–442、560–590 行）；入参是中性的
//! [`Call`]。
use super::text::{
    self, find_figure, first_line, grep_count, jtrim, line_count, lines, lines_iter, num_of,
    patch_files, plural, relative, shell_lines, str_of,
};
use super::{
    Call, DiffRow, Ink, OUTPUTS, Part, Sign, Status, Steps, TAIL_ROWS, Under, Where, background,
    call_label, current, exit_code, failed, input, mcp, mcp_error, nowhere, output, time,
    timed_out, todo_change, tool,
};

/// 调用结束后发生了什么。输出的形状不认识时为空：不猜。`before` 是 `Todos` 调用（按 id）找到的清单。
pub fn outcome(c: &Call<'_>, before: &dyn Fn(&str) -> Option<Steps>) -> Vec<Part> {
    if c.status == Status::Cancelled {
        return vec![Part::new("stopped", Ink::Fg3)];
    }
    let t = tool(c);
    if t != "Bash" && failed(c) {
        return vec![Part::new("failed", Ink::Err)];
    }
    let out = output(c);
    let i = input(c);
    let fg3 = |s: String| vec![Part::new(s, Ink::Fg3)];
    match t {
        "Read" => {
            let numbers: Vec<i64> = lines_iter(out)
                .into_iter()
                .filter_map(|l| {
                    let (d, n) = text::digits(l)?;
                    l[n..].starts_with('\t').then(|| d.parse().ok()).flatten()
                })
                .collect();
            match (numbers.first(), numbers.last()) {
                (Some(a), Some(b)) => {
                    if num_of(i, "offset").is_some() || num_of(i, "limit").is_some() {
                        fg3(format!("lines {a}–{b}"))
                    } else {
                        fg3(plural(*b, "line"))
                    }
                }
                _ => Vec::new(),
            }
        }
        "Grep" => grep_count(out, str_of(i, "output_mode")).map_or_else(Vec::new, fg3),
        "Glob" => {
            if jtrim(out) == "No files found" {
                fg3("no files".to_owned())
            } else {
                let files = lines_iter(out)
                    .into_iter()
                    .map(jtrim)
                    .filter(|l| !l.is_empty() && !l.starts_with("(Results are truncated"))
                    .count();
                if files == 0 {
                    Vec::new()
                } else {
                    fg3(plural(files as i64, "file"))
                }
            }
        }
        "WebSearch" => {
            if out.starts_with("Web search results") {
                let n = lines_iter(out)
                    .into_iter()
                    .filter(|l| l.starts_with("- ["))
                    .count();
                fg3(plural(n as i64, "result"))
            } else {
                Vec::new()
            }
        }
        "WebFetch" => out
            .strip_prefix("The server returned HTTP ")
            .and_then(text::digits)
            .map_or_else(Vec::new, |(d, _)| {
                vec![Part::new(format!("HTTP {d}"), Ink::Err)]
            }),
        "Edit" | "Write" | "NotebookEdit" => match c.facts.and_then(|f| f.diff) {
            Some(d) => counted(d.added as i64, d.removed as i64),
            None if out.starts_with("File created successfully") => fg3(format!(
                "new · {}",
                plural(line_count(str_of(i, "content")), "line")
            )),
            None => Vec::new(),
        },
        "ApplyPatch" => {
            let files = patch_files(str_of(i, "patch_text"), &nowhere());
            if files.is_empty() {
                Vec::new()
            } else {
                counted(
                    files.iter().map(|f| f.1).sum(),
                    files.iter().map(|f| f.2).sum(),
                )
            }
        }
        "Bash" => {
            let spent = took(c);
            let after = |head: Part| {
                let mut v = vec![head];
                v.extend(spent.iter().map(|s| Part::new(format!(" · {s}"), Ink::Fg3)));
                v
            };
            if background(c) {
                fg3("in the background".to_owned())
            } else if c.status == Status::Error {
                vec![Part::new("failed", Ink::Err)]
            } else if timed_out(c) {
                after(Part::new("timed out", Ink::Err))
            } else {
                match exit_code(c) {
                    Some(n) => after(Part::new(format!("exit {n}"), Ink::Err)),
                    None => spent.map_or_else(Vec::new, fg3),
                }
            }
        }
        o if OUTPUTS.contains(&o) => c
            .facts
            .map(|f| f.task_status)
            .filter(|s| !s.is_empty())
            .map_or_else(Vec::new, |s| fg3(s.to_owned())),
        "TodoWrite" => {
            let ch = todo_change(c, before(c.id).as_ref());
            if ch.new_list {
                Vec::new()
            } else {
                fg3(format!("{} of {}", ch.done(), ch.steps.len()))
            }
        }
        "Agent" | "Task" => {
            if background(c) {
                fg3("in the background".to_owned())
            } else {
                // 用时、token 与速度在行尾的数字列里（§4.2「子代理的数字」），结果只说用了几次工具。
                find_figure(out, "tool_uses")
                    .map_or_else(Vec::new, |n| fg3(plural(n as i64, "tool")))
            }
        }
        _ => Vec::new(),
    }
}

/// 在跑的行的末尾说的：命令过了 3 秒的秒数、MCP 调用的进度。
pub fn live(c: &Call<'_>, now_ms: f64) -> Vec<Part> {
    let Some(f) = c.facts else {
        return Vec::new();
    };
    match f.progress {
        Some(p) if f.mcp.is_some() => {
            let pct = (p.fraction >= 0.0).then(|| {
                Part::new(
                    format!("{}%", (p.fraction * 100.0 + 0.5).floor() as i64),
                    Ink::Fg2,
                )
            });
            let lead = if pct.is_some() { " · " } else { "" };
            let msg = (!p.message.is_empty())
                .then(|| Part::new(format!("{lead}{}", p.message), Ink::Fg3));
            pct.into_iter().chain(msg).collect()
        }
        _ => {
            let secs = ((now_ms - f.started_at as f64) / 1000.0).trunc() as i64;
            if f.tool == "Bash" && f.started_at > 0 && secs >= 3 {
                vec![Part::new(format!("· {secs}s"), Ink::Fg3)]
            } else {
                Vec::new()
            }
        }
    }
}

/// 展开（^O）时挂在行下面的。收在一段的行里时，只有在跑的调用下面有行：`details` 为假，命令给出最后 [`TAIL_ROWS`] 行
/// 而不是全部（§4.2「工作的收拢」）。
pub fn under(
    c: &Call<'_>,
    details: bool,
    where_: &Where,
    before: &dyn Fn(&str) -> Option<Steps>,
) -> Under {
    // 排队的调用还没有证据（§4.2「工具调用」的「行」）。
    let Some(f) = c.facts else {
        return Under::Nothing;
    };
    if matches!(c.status, Status::Asking | Status::Queued) {
        return Under::Nothing;
    }
    let ended = c.ended;
    let i = f.input;
    if c.status == Status::Error {
        return Under::Reason(lines(f.error, false));
    }
    if let Some(why) = mcp_error(c) {
        return Under::Reason(lines(&why, false));
    }
    match f.tool {
        "Edit" | "Write" | "NotebookEdit" if ended => match f.diff {
            Some(d) => Under::Diff(
                d.lines
                    .iter()
                    .map(|l| DiffRow {
                        n: l.line as i64,
                        sign: match l.sign {
                            Sign::Add => '+',
                            Sign::Remove => '-',
                            Sign::Context => ' ',
                        },
                        text: text::clean(&l.text),
                    })
                    .collect(),
            ),
            None if f.output.starts_with("File created successfully") => Under::Diff(
                lines(str_of(i, "content"), true)
                    .into_iter()
                    .enumerate()
                    .map(|(k, l)| DiffRow {
                        n: k as i64 + 1,
                        sign: '+',
                        text: l,
                    })
                    .collect(),
            ),
            None => Under::Nothing,
        },
        "ApplyPatch" if ended => Under::Files(patch_files(str_of(i, "patch_text"), where_)),
        "Bash" => {
            if background(c) {
                return Under::Nothing;
            }
            let all = if ended {
                shell_lines(f.output)
            } else {
                lines(f.preview, true)
                    .into_iter()
                    .filter(|l| !jtrim(l).is_empty())
                    .collect()
            };
            if details {
                Under::Shell {
                    command: first_line(str_of(i, "command")),
                    why: jtrim(str_of(i, "description")).to_owned(),
                    lines: all,
                }
            } else {
                let skip = all.len().saturating_sub(TAIL_ROWS);
                Under::Tail(
                    all[skip..]
                        .iter()
                        .map(|l| relative(l, &where_.cwd))
                        .collect(),
                )
            }
        }
        "Agent" | "Task" => {
            if !ended {
                current(c).map_or(Under::Nothing, |cur| Under::Ticker(call_label(cur, where_)))
            } else if background(c) {
                Under::Nothing
            } else {
                let report: Vec<String> = lines(f.output, false)
                    .into_iter()
                    .take_while(|l| !l.starts_with("agentId:") && !l.starts_with("<usage>"))
                    .collect();
                if report.is_empty() {
                    Under::Nothing
                } else {
                    Under::Report(report)
                }
            }
        }
        "js" if ended => report(f.output),
        // 新的清单画出全部步骤；其余的更新只是一行，清单不重画（§4.2「工具调用」的「证据」）。
        "TodoWrite" if ended => {
            let ch = todo_change(c, before(c.id).as_ref());
            if ch.new_list {
                Under::Todos(ch.steps)
            } else {
                Under::Nothing
            }
        }
        _ if ended && mcp(c).is_some() => report(f.output),
        _ => Under::Nothing,
    }
}

fn report(out: &str) -> Under {
    let ls = lines(out, false);
    if ls.is_empty() {
        Under::Nothing
    } else {
        Under::Report(ls)
    }
}

/// 它用了多久；不到一秒不说：`0.0s` 什么都没说（§4.2「结果」）。
fn took(c: &Call<'_>) -> Option<String> {
    c.facts
        .filter(|f| f.started_at > 0 && f.ended_at.saturating_sub(f.started_at) >= 1000)
        .map(|f| time(f.ended_at as f64 - f.started_at as f64))
}

fn counted(added: i64, removed: i64) -> Vec<Part> {
    let a = (added > 0).then(|| Part::new(format!("+{added}"), Ink::Ok));
    let d = (removed > 0).then(|| Part::new(format!("−{removed}"), Ink::Err));
    match (a, d) {
        (Some(x), Some(y)) => vec![x, Part::new(" ", Ink::Fg3), y],
        (a, d) => a.into_iter().chain(d).collect(),
    }
}
