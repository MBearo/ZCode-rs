//! 收拢的一行的句子（Gather.scala 第 190–290 行）：子句、句子、短的说法与放不下时怎样缩。
//! 移植自 Puffin 的 shell/gather/words.rs；入参是中性的 [`Member`]。
use crate::render::{Span, Style};
use crate::theme::theme::Theme;

use super::super::fit::{self, cut, width};
use super::super::tools::{Tally, text::plural};
use super::Member;

/// 一个子句：句子里的动词与其余，和它短的说法（`read 8`）。`key` 是它在 [`DROP_ORDER`] 里的名字。`extra` 只在完整的句子里
/// 跟在 `rest` 后面，是放不下的句子先去掉的。
#[derive(Clone, Debug)]
pub struct Clause {
    pub key: &'static str,
    pub verb: String,
    pub rest: Vec<Span>,
    pub short: Vec<Span>,
    pub extra: Vec<Span>,
}

fn clause(key: &'static str, verb: &str, rest: Vec<Span>, short: Vec<Span>) -> Clause {
    Clause {
        key,
        verb: verb.to_owned(),
        rest,
        short,
        extra: Vec::new(),
    }
}

fn n(i: usize) -> i64 {
    i as i64
}

/// 收拢的一行里的字：动词、数名词的说法、连接的话与失败。动词按句子里的样子写（句首由 [`capitalize`] 改大写）；带前导
/// 空格的话照原样接。别的界面语言给译文；放不下时怎样缩不变。
pub struct GatherWords {
    /// `k` 个 `noun`（`file`、`time`、`page`、`command`、`subagent`、`tool`、`call`、`step`）：`4 files`。
    pub count: fn(i64, &str) -> String,
    pub thought: &'static str,
    /// 想了多久，接在 `thought` 后面：` for 9s`。
    pub for_time: fn(&str) -> String,
    pub explored: &'static str,
    /// 短的说法里的 `explored`。
    pub read: &'static str,
    pub searched: &'static str,
    pub fetched: &'static str,
    pub searched_web: &'static str,
    pub edited: &'static str,
    pub ran: &'static str,
    pub planned: &'static str,
    pub checked_off: &'static str,
    pub called: &'static str,
    pub stopped: &'static str,
    pub updated: &'static str,
    /// 接在 `updated` 后面：` the steps`。
    pub the_steps: &'static str,
    /// 两样之间：` and `。
    pub and: &'static str,
    /// 子句之间：`, `。
    pub comma: &'static str,
    /// 子代理的 token，接在 `ran` 的子句后面：` (309k tokens)`。
    pub tokens: fn(&str) -> String,
    /// `who` 失败了：`Bash failed`、`1 failed`。
    pub failed: fn(&str) -> String,
    /// 同一个调用几次：`Read 2 times`。
    pub times: fn(&str, usize) -> String,
    /// 一批调用在外面、没有别的可说时：`3 calls at once`。
    pub at_once: fn(usize) -> String,
    /// 此刻在想：`Thinking`。
    pub thinking: &'static str,
}

/// Puffin 的字。
pub static GATHER_WORDS: GatherWords = GatherWords {
    count: plural,
    thought: "thought",
    for_time: |x| format!(" for {x}"),
    explored: "explored",
    read: "read",
    searched: "searched",
    fetched: "fetched",
    searched_web: "searched the web",
    edited: "edited",
    ran: "ran",
    planned: "planned",
    checked_off: "checked off",
    called: "called",
    stopped: "stopped",
    updated: "updated",
    the_steps: " the steps",
    and: " and ",
    comma: ", ",
    tokens: |x| format!(" ({x} tokens)"),
    failed: |who| format!("{who} failed"),
    times: |name, k| format!("{name} {k} times"),
    at_once: |k| format!("{k} calls at once"),
    thinking: "Thinking",
};

/// 子句，按固定的次序：思考、读、改、命令、待办、别的调用、停下的调用。
pub fn clauses(theme: &Theme, members: &[Member<'_>], t: &Tally, w: &GatherWords) -> Vec<Clause> {
    let plural = w.count;
    let quiet = |text: String| Span::new(text, theme.fg3);
    let verb = |text: &str| Span::new(text.to_owned(), theme.fg2);
    let counted = |key, word: &str, k: i64, noun: &str, short_word: &str| {
        clause(
            key,
            word,
            vec![quiet(format!(" {}", plural(k, noun)))],
            vec![verb(short_word), quiet(format!(" {k}"))],
        )
    };
    let listed = |key, word: &str, names: &[String], noun: &str| {
        let what = vec![quiet(format!(" {}", named(names, noun, w)))];
        let mut short = vec![verb(word)];
        short.extend(what.iter().cloned());
        clause(key, word, what, short)
    };
    let thoughts: Vec<u64> = members
        .iter()
        .filter_map(|m| match m {
            Member::Thought(th) if th.ended => Some(th.duration_ms.unwrap_or(0)),
            _ => None,
        })
        .collect();
    let any_thought = members
        .iter()
        .any(|m| matches!(m, Member::Thought(th) if th.ended));
    let ms: u64 = thoughts.iter().sum();
    let time = (any_thought && ms > 0).then(|| fit::thought_time(ms as f64));
    let mut counts = Vec::new();
    if t.added > 0 {
        counts.push(Span::new(" ", Style::DEFAULT));
        counts.push(Span::new(format!("+{}", t.added), Style::fg(theme.ok)));
    }
    if t.removed > 0 {
        counts.push(Span::new(" ", Style::DEFAULT));
        counts.push(Span::new(format!("−{}", t.removed), Style::fg(theme.err)));
    }
    let mut out: Vec<Clause> = Vec::new();
    if any_thought {
        let rest = time.iter().map(|x| quiet((w.for_time)(x))).collect();
        let mut short = vec![verb(w.thought)];
        short.extend(time.iter().map(|x| quiet(format!(" {x}"))));
        out.push(clause("thought", w.thought, rest, short));
    }
    if t.files > 0 {
        out.push(counted("read", w.explored, t.files, "file", w.read));
    } else if t.searches > 0 {
        out.push(counted("read", w.searched, t.searches, "time", w.searched));
    } else if t.pages > 0 {
        out.push(counted("read", w.fetched, t.pages, "page", w.fetched));
    } else if t.web_searches > 0 {
        out.push(counted(
            "read",
            w.searched_web,
            t.web_searches,
            "time",
            w.searched_web,
        ));
    }
    if !t.edited.is_empty() {
        let what = if t.edited.len() == 1 {
            t.edited[0].clone()
        } else {
            plural(n(t.edited.len()), "file")
        };
        let mut rest = vec![quiet(format!(" {what}"))];
        rest.extend(counts.iter().cloned());
        let mut short = vec![verb(w.edited), quiet(format!(" {}", t.edited.len()))];
        short.extend(counts);
        out.push(clause("edited", w.edited, rest, short));
    }
    out.extend(ran(theme, t, w));
    out.extend(todos(theme, t, w));
    if !t.others.is_empty() {
        out.push(listed("called", w.called, &t.others, "tool"));
    }
    // 被停下的调用不算进别的子句，在最后说出（回合末尾还有「Stopped.」）。
    if !t.stopped.is_empty() {
        out.push(listed("stopped", w.stopped, &t.stopped, "call"));
    }
    // 只开始了一步的更新不说；段里只有它们时句子不能是空的。
    if out.is_empty() && t.todo_updates > 0 {
        out.push(clause(
            "todos",
            w.updated,
            vec![quiet(w.the_steps.to_owned())],
            vec![verb(w.updated), quiet(w.the_steps.to_owned())],
        ));
    }
    out
}

/// 命令与子代理，一个子句：`ran 2 commands and 3 subagents (309k tokens)`，短的 `ran 2 and 3 subagents`。token 是子代理报告
/// 的（§4.2「子代理的数字」）；短的说法里没有。
fn ran(theme: &Theme, t: &Tally, w: &GatherWords) -> Option<Clause> {
    let plural = w.count;
    let subagents = (t.subagents > 0).then(|| plural(t.subagents, "subagent"));
    if t.commands == 0 && subagents.is_none() {
        return None;
    }
    let full: Vec<String> = (t.commands > 0)
        .then(|| plural(t.commands, "command"))
        .into_iter()
        .chain(subagents.clone())
        .collect();
    let short: Vec<String> = (t.commands > 0)
        .then(|| t.commands.to_string())
        .into_iter()
        .chain(subagents)
        .collect();
    let mut c = clause(
        "ran",
        w.ran,
        vec![Span::new(format!(" {}", full.join(w.and)), theme.fg3)],
        vec![
            Span::new(w.ran, theme.fg2),
            Span::new(format!(" {}", short.join(w.and)), theme.fg3),
        ],
    );
    if t.subagents > 0 && t.subagent_tokens > 0.0 {
        c.extra = vec![Span::new(
            (w.tokens)(&fit::count(t.subagent_tokens)),
            theme.fg3,
        )];
    }
    Some(c)
}

/// 待办清单，一个子句：`planned 6 steps`、`checked off 2 steps` 或 `planned 6 steps and checked off 1`；短的
/// `planned 6 · checked off 1`。
fn todos(theme: &Theme, t: &Tally, w: &GatherWords) -> Option<Clause> {
    let plural = w.count;
    let quiet = |text: String| Span::new(text, theme.fg3);
    let verb = |text: &str| Span::new(text.to_owned(), theme.fg2);
    match (t.planned, t.checked_off) {
        (0, 0) => None,
        (p, 0) => Some(clause(
            "todos",
            w.planned,
            vec![quiet(format!(" {}", plural(p, "step")))],
            vec![verb(w.planned), quiet(format!(" {p}"))],
        )),
        (0, c) => Some(clause(
            "todos",
            w.checked_off,
            vec![quiet(format!(" {}", plural(c, "step")))],
            vec![verb(w.checked_off), quiet(format!(" {c}"))],
        )),
        (p, c) => Some(clause(
            "todos",
            w.planned,
            vec![
                quiet(format!(" {}{}", plural(p, "step"), w.and)),
                verb(w.checked_off),
                quiet(format!(" {c}")),
            ],
            vec![
                verb(w.planned),
                quiet(format!(" {p}")),
                Span::new(" · ", theme.line),
                verb(w.checked_off),
                quiet(format!(" {c}")),
            ],
        )),
    }
}

/// Scala 的 `capitalize`：第一个字母大写。
fn capitalize(s: &str) -> String {
    let mut cs = s.chars();
    match cs.next() {
        Some(c) => c.to_uppercase().chain(cs).collect(),
        None => String::new(),
    }
}

/// 句子：`Thought for 9s, explored 4 files · 1 failed`；没有别的可说时是失败的。失败只在末尾数。
fn sentence(theme: &Theme, said: &[Clause], t: &Tally, extras: bool, w: &GatherWords) -> Vec<Span> {
    let mut words = Vec::new();
    for (i, c) in said.iter().enumerate() {
        if i > 0 {
            words.push(Span::new(w.comma, theme.fg3));
        }
        let v = if i == 0 {
            capitalize(&c.verb)
        } else {
            c.verb.clone()
        };
        words.push(Span::new(v, theme.fg2));
        words.extend(c.rest.iter().cloned());
        if extras {
            words.extend(c.extra.iter().cloned());
        }
    }
    let failed = t.failed.len();
    let err = Style::fg(theme.err);
    if failed == 0 {
        words
    } else if words.is_empty() {
        let who = if failed == 1 {
            t.failed[0].clone()
        } else {
            (w.count)(n(failed), "call")
        };
        vec![Span::new((w.failed)(&who), err)]
    } else {
        words.push(Span::new(" · ", theme.line));
        words.push(Span::new((w.failed)(&failed.to_string()), err));
        words
    }
}

/// 短的说法：`thought 21s · read 8 · edited 4 +44 −10 · ran 3 · 1 failed`，去掉 `drop` 里的子句，有去掉的时先是 `…`。
fn short(theme: &Theme, said: &[Clause], t: &Tally, drop: &[&str], w: &GatherWords) -> Vec<Span> {
    let kept: Vec<&Clause> = said.iter().filter(|c| !drop.contains(&c.key)).collect();
    let mut parts: Vec<Vec<Span>> = Vec::new();
    if kept.len() < said.len() {
        parts.push(vec![Span::new("…", theme.fg3)]);
    }
    parts.extend(kept.iter().map(|c| c.short.clone()));
    if !t.failed.is_empty() {
        parts.push(vec![Span::new(
            (w.failed)(&t.failed.len().to_string()),
            Style::fg(theme.err),
        )]);
    }
    let mut out = Vec::new();
    for (i, p) in parts.into_iter().enumerate() {
        if i > 0 {
            out.push(Span::new(" · ", theme.line));
        }
        out.extend(p);
    }
    out
}

/// 短的说法先去掉的：最不说明问题的。停下的调用与失败从不去掉。
const DROP_ORDER: [&str; 6] = ["thought", "todos", "called", "read", "ran", "edited"];

/// 这段做了的，至多 `room` 格（§4.2「工作的收拢」的「放不下」）：句子；否则不带附加（子代理的 token）的句子；否则短的
/// 说法；否则去掉最不说明问题的子句的短说法，至少留一个；否则用 `…` 截断的短说法。
pub fn fit(theme: &Theme, said: &[Clause], t: &Tally, room: i64, w: &GatherWords) -> Vec<Span> {
    let full = sentence(theme, said, t, true, w);
    if width(&full) as i64 <= room || said.is_empty() {
        return cut(&full, room);
    }
    let bare = sentence(theme, said, t, false, w);
    if width(&bare) as i64 <= room {
        return bare;
    }
    let droppable: Vec<&str> = DROP_ORDER
        .iter()
        .copied()
        .filter(|k| said.iter().any(|c| c.key == *k))
        .collect();
    let most = if said.iter().all(|c| droppable.contains(&c.key)) {
        droppable.len().saturating_sub(1)
    } else {
        droppable.len()
    };
    (0..=most)
        .map(|k| short(theme, said, t, &droppable[..k], w))
        .find(|s| width(s) as i64 <= room)
        .unwrap_or_else(|| cut(&short(theme, said, t, &[], w), room))
}

/// 按名字说调用：`A`、`A 2 times`、`A and B`；更多时 `N tools`。
fn named(names: &[String], noun: &str, w: &GatherWords) -> String {
    let mut distinct: Vec<&String> = Vec::new();
    for x in names {
        if !distinct.contains(&x) {
            distinct.push(x);
        }
    }
    if distinct.len() == 1 {
        if names.len() == 1 {
            names[0].clone()
        } else {
            (w.times)(&names[0], names.len())
        }
    } else if distinct.len() == 2 && names.len() == 2 {
        format!("{}{}{}", names[0], w.and, names[1])
    } else {
        (w.count)(names.len() as i64, noun)
    }
}
