//! 设置的 Permissions 段（docs/specs/rust-cli/tui-kit.md §4.2「设置」；照抄 Puffin `shell/settings/permissions.rs`，两套界面
//! 不能互相依赖）：权限规则按三组列出，本项目的规则能删、能改种类与规则串，配置文件的规则在编辑器里打开那个文件；添加页把
//! 打的字换算成规则串，跟着字说它存成什么、意味着什么。规则串的解析与写回是 kernel-api 的 `permission::syntax`
//! （`parse_rule` / `format_rule`，与 hook 的条件同一套语法）。
use std::sync::Arc;

use rust_cli_kernel_api::permission::syntax::{format_rule, parse_rule};
use rust_cli_kernel_api::permission::{PermissionRule, RuleBehavior, RuleSource, SourcedRule};

use super::data::Permissions as PermissionsSettings;
use super::page::{Act, Change as SettingsChange, Ctx, ItemRow, Page, Row, TextRow, View, Write};
use rust_cli_tui_kit::app::widgets::settings::{Applies, Ink, Part};

use rust_cli_tui_kit::app::widgets::settings::rules::{self as words, Behavior, Source};

// 措辞搬进 kit 的 `app/widgets/settings/rules.rs`（docs/specs/rust-cli/tui-kit.md §4.4 K5）；这里把 kernel-api 的规则
// 映射成 kit 的中性的 `Rule`、`Behavior` 与 `Source`。
use words::{ADD, EMPTY, NEEDS_RULE, NEW_DESC, RULE_HINT};

/// 添加规则的表单的键。
const NEW_KIND: &str = "nr.kind";
const NEW_RULE: &str = "nr.rule";
/// 段上 `+ Add a rule` 的行 id。
pub const ADD_ROW: &str = "rule:+";

fn behavior(b: RuleBehavior) -> Behavior {
    match b {
        RuleBehavior::Allow => Behavior::Allow,
        RuleBehavior::Ask => Behavior::Ask,
        RuleBehavior::Deny => Behavior::Deny,
    }
}

fn rule_behavior(b: Behavior) -> RuleBehavior {
    match b {
        Behavior::Allow => RuleBehavior::Allow,
        Behavior::Ask => RuleBehavior::Ask,
        Behavior::Deny => RuleBehavior::Deny,
    }
}

fn source(s: RuleSource) -> Source {
    match s {
        RuleSource::UserSettings => Source::UserSettings,
        RuleSource::ProjectSettings => Source::ProjectSettings,
        RuleSource::LocalSettings => Source::LocalSettings,
        RuleSource::CliArg => Source::CliArg,
        RuleSource::Session => Source::Session,
    }
}

/// `rule` 作为 kit 的中性的规则（规则串由 `format_rule` 写回）。
fn with_rule<R>(rule: &PermissionRule, f: impl FnOnce(words::Rule<'_>) -> R) -> R {
    let text = format_rule(rule);
    f(words::Rule {
        tool: &rule.tool,
        content: rule.content.as_deref(),
        text: &text,
    })
}

fn behavior_value(b: RuleBehavior) -> &'static str {
    words::behavior_value(behavior(b))
}

fn behavior_of(value: &str) -> Option<RuleBehavior> {
    words::behavior_of(value).map(rule_behavior)
}

fn behavior_name(b: RuleBehavior) -> &'static str {
    words::behavior_name(behavior(b))
}

/// 段里一条规则的行 id，也是它的页的身份。
pub fn row_id(r: &SourcedRule) -> String {
    format!(
        "rule:{}:{}:{}",
        words::source_value(source(r.source)),
        behavior_value(r.behavior),
        format_rule(&r.rule)
    )
}

/// 本项目的一条规则（新加的或改过的）的行 id。
fn local_id(behavior: RuleBehavior, rule: &str) -> String {
    row_id(&SourcedRule {
        source: RuleSource::LocalSettings,
        behavior,
        rule: parse_rule(rule),
    })
}

/// Bash 规则的前缀：`X:*` 或 `X *` 的 X。
fn prefix(content: &str) -> Option<&str> {
    words::prefix(content)
}

/// 规则的说法，与请求的 Always allow 同一套：命令是前缀加 ` …`，Edit 是 `edits to …`。
pub fn phrase(rule: &PermissionRule) -> String {
    with_rule(rule, words::phrase)
}

fn means(rule: &PermissionRule) -> Vec<Part> {
    with_rule(rule, words::means)
}

fn sentence(r: &SourcedRule) -> String {
    with_rule(&r.rule, |k| words::sentence(behavior(r.behavior), k))
}

fn page_desc(r: &SourcedRule) -> String {
    with_rule(&r.rule, |k| words::page_desc(behavior(r.behavior), k))
}

fn removal_note(r: &SourcedRule) -> String {
    with_rule(&r.rule, |k| words::removal_note(behavior(r.behavior), k))
}

fn removed_said(r: &SourcedRule) -> String {
    with_rule(&r.rule, |k| words::removed_said(behavior(r.behavior), k))
}

/// 删一条规则：先问（缺省 Keep it）。`select` 是之后选中的行，`back` 是从规则页删的。
fn remove_act(r: &SourcedRule, select: Option<String>, back: bool) -> Act {
    let mut w = Write::new(SettingsChange::RemoveRule { rule: r.clone() }, Applies::Now)
        .saying(removed_said(r));
    if back {
        w = w.going_back();
    }
    if let Some(id) = select {
        w = w.selecting(id);
    }
    Act::Confirm {
        label: "Remove".to_owned(),
        note: removal_note(r),
        next: Box::new(Act::Write(w)),
    }
}

/// 改种类或规则串：host 先加新的、再删旧的。一样的规则不动（加了再删会把它删掉）。
fn replace_act(old: &SourcedRule, behavior: RuleBehavior, rule: String) -> Act {
    if behavior == old.behavior && parse_rule(&rule) == old.rule {
        return Act::Nothing;
    }
    let id = local_id(behavior, &rule);
    Act::Write(
        Write::new(
            SettingsChange::ReplaceRule {
                old: old.clone(),
                behavior,
                rule,
            },
            Applies::Now,
        )
        .saying(words::replaced_said())
        .swapping(Page::Rule(id)),
    )
}

/// 打的字换算成规则串：网址是那个主机的页，`工具(…)` 与工具名照写，其余是命令的前缀（spec §11.9）。
pub fn convert(text: &str) -> Result<String, &'static str> {
    let t = text.trim();
    if t.is_empty() {
        return Err(NEEDS_RULE);
    }
    let lower = t.to_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        let rest = &t[t.find("://").map_or(0, |i| i + 3)..];
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        let at = authority.rsplit('@').next().unwrap_or("");
        let host = if at.starts_with('[') {
            at.split_inclusive(']').next().unwrap_or("")
        } else {
            at.split(':').next().unwrap_or("")
        };
        if host.is_empty() {
            return Err("The address has no host");
        }
        return Ok(format!("WebFetch(domain:{})", host.to_lowercase()));
    }
    let tool_name =
        |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if let Some(open) = t.find('(')
        && tool_name(&t[..open])
    {
        let slashes = t[..t.len() - 1]
            .bytes()
            .rev()
            .take_while(|b| *b == b'\\')
            .count();
        if !t.ends_with(')') || t.len() - 1 == open || slashes % 2 == 1 {
            return Err("The rule needs a closing )");
        }
        return Ok(format_rule(&parse_rule(t)));
    }
    if t.starts_with("mcp__") && !t.contains(char::is_whitespace)
        || t.starts_with(|c: char| c.is_ascii_uppercase()) && tool_name(t)
    {
        return Ok(format_rule(&parse_rule(t)));
    }
    let content = if prefix(t).is_some() {
        t.to_owned()
    } else {
        format!("{t}:*")
    };
    Ok(format_rule(&PermissionRule {
        tool: "Bash".to_owned(),
        content: Some(content),
    }))
}

/// 段里的次序：组，组里本项目的在前，各自按说法（不分大小写）。
fn ordered(p: &PermissionsSettings) -> Vec<&SourcedRule> {
    let group = |b: RuleBehavior| {
        words::GROUPS
            .iter()
            .position(|(x, _)| *x == behavior(b))
            .unwrap_or(0)
    };
    let mut rules: Vec<&SourcedRule> = p.rules.iter().collect();
    rules.sort_by_cached_key(|r| {
        (
            group(r.behavior),
            r.source != RuleSource::LocalSettings,
            phrase(&r.rule).to_lowercase(),
        )
    });
    rules
}

/// 配置文件来源的规则来自的文件。
fn file_of<'a>(p: &'a PermissionsSettings, r: &SourcedRule) -> Option<&'a str> {
    p.files
        .iter()
        .find(|f| f.source == r.source && f.behavior == r.behavior)
        .map(|f| f.path.as_str())
}

/// 工作区根。
fn root(p: &PermissionsSettings) -> Option<&str> {
    p.folders.iter().find(|f| !f.added).map(|f| f.path.as_str())
}

/// 文件的说法：工作区里的写相对路径，否则家目录写成 `~`。
fn shown_path(ctx: &Ctx<'_>, p: &PermissionsSettings, path: &str) -> String {
    root(p)
        .filter(|r| !r.is_empty())
        .and_then(|r| {
            path.strip_prefix(r)
                .and_then(|rest| rest.strip_prefix(['/', '\\']))
        })
        .map_or_else(|| ctx.tilde(path), str::to_owned)
}

fn kind_row(
    key: &str,
    current: RuleBehavior,
    pick: impl Fn(&str) -> Act + Send + Sync + 'static,
) -> Row {
    Row::Choice {
        key: key.to_owned(),
        label: "Kind".to_owned(),
        options: words::kind_options(),
        current: behavior_value(current).to_owned(),
        // 种类没有缺省值之说：行上不画 `default`（设计稿）。
        default: String::new(),
        desc: String::new(),
        applies: Applies::Form,
        pick: Arc::new(pick),
    }
}

/// 一条规则在段里的行。`next` 是删掉它之后选中的行。
fn rule_row(r: &SourcedRule, next: String, p: &PermissionsSettings, ctx: &Ctx<'_>) -> Row {
    let id = row_id(r);
    let config = words::from_config(source(r.source));
    let file = file_of(p, r);
    let desc = if config {
        words::config_desc(file.map(|f| shown_path(ctx, p, f)).as_deref())
    } else {
        sentence(r)
    };
    let open = if config {
        file.map(|f| Act::Edit(f.to_owned()))
    } else {
        Some(Act::Push(Page::Rule(id.clone())))
    };
    Row::item(ItemRow {
        key: id,
        name: phrase(&r.rule),
        toggle: None,
        scope: words::source_word(source(r.source)).to_owned(),
        status: vec![Part::new(words::kind_word(&r.rule.tool), Ink::Fg3)],
        desc,
        open,
        flip: None,
        detail: Vec::new(),
        remove: (!config).then(|| remove_act(r, Some(next), false)),
        enter: if config { words::OPEN_FILE } else { "" }.to_owned(),
        words: format_rule(&r.rule),
    })
}

/// 段自己的行：三组规则、`+ Add a rule`、工作的文件夹。
pub fn section(p: &PermissionsSettings, ctx: &Ctx<'_>) -> Vec<Row> {
    let rules = ordered(p);
    let ids: Vec<String> = rules.iter().map(|r| row_id(r)).collect();
    // 删掉一条之后选中下一条；最后一条删掉时选中上一条；一条都不剩时是 `+ Add a rule`。
    let next = |i: usize| {
        ids.get(i + 1)
            .or_else(|| i.checked_sub(1).and_then(|j| ids.get(j)))
            .cloned()
            .unwrap_or_else(|| ADD_ROW.to_owned())
    };
    let mut rows = Vec::new();
    if rules.is_empty() {
        rows.push(Row::Note(EMPTY.to_owned()));
    }
    for (group, name) in words::GROUPS {
        let mut heading = false;
        for (i, r) in rules
            .iter()
            .enumerate()
            .filter(|(_, r)| behavior(r.behavior) == group)
        {
            if !heading {
                rows.push(Row::Heading(name.to_owned()));
                heading = true;
            }
            rows.push(rule_row(r, next(i), p, ctx));
        }
    }
    rows.push(Row::Note(String::new()));
    rows.push(Row::plus(
        ADD_ROW,
        "Add a rule",
        ADD,
        Act::Push(Page::NewRule),
    ));
    if !p.folders.is_empty() {
        rows.push(Row::Heading(words::FOLDERS.to_owned()));
    }
    for f in &p.folders {
        let (scope, desc) = words::folder_words(f.added);
        rows.push(Row::item(ItemRow {
            key: format!("folder:{}", f.path),
            name: ctx.tilde(&f.path),
            toggle: None,
            scope: scope.to_owned(),
            status: Vec::new(),
            desc: desc.to_owned(),
            open: None,
            flip: None,
            detail: Vec::new(),
            remove: None,
            enter: String::new(),
            words: String::new(),
        }));
    }
    rows
}

/// 一条规则的页：本项目的能改种类与规则串；命令行与会话的只能删。
pub fn rule(id: &str, ctx: &Ctx<'_>) -> View {
    let Some((p, r)) = ctx.permissions().and_then(|p| {
        p.rules
            .iter()
            .find(|r| row_id(r) == id)
            .map(|r| (p, r.clone()))
    }) else {
        return View::loading("Permissions");
    };
    let mut rows = Vec::new();
    if r.source == RuleSource::LocalSettings {
        let old = r.clone();
        let current = format_rule(&r.rule);
        rows.push(kind_row("r.kind", r.behavior, {
            let current = current.clone();
            move |v| behavior_of(v).map_or(Act::Nothing, |b| replace_act(&old, b, current.clone()))
        }));
        let old = r.clone();
        let mut text =
            TextRow::new(
                "r.rule",
                "Rule",
                current,
                "",
                Applies::Form,
                move |v| match convert(v) {
                    Ok(rule) => replace_act(&old, old.behavior, rule),
                    Err(why) => Act::Refuse(why.to_owned()),
                },
            );
        text.hint = RULE_HINT.to_owned();
        text.stuck = ctx.draft("r.rule").is_some_and(|d| convert(d).is_err());
        rows.push(Row::Text(text));
        let place = root(p).map(|r| ctx.tilde(r));
        rows.push(Row::info(
            "Kept",
            vec![Part::new(words::kept_local(place.as_deref()), Ink::Fg3)],
        ));
    } else {
        rows.push(Row::info(
            "Kind",
            vec![Part::new(behavior_name(r.behavior), Ink::Fg)],
        ));
        rows.push(Row::info(
            "Rule",
            vec![Part::new(format_rule(&r.rule), Ink::Fg)],
        ));
        let kept = words::kept_other(source(r.source));
        rows.push(Row::info("Kept", vec![Part::new(kept, Ink::Fg3)]));
    }
    rows.push(Row::Note(String::new()));
    rows.push(Row::danger(
        "r.remove",
        "Remove this rule",
        remove_act(&r, None, true),
    ));
    View::new(vec![phrase(&r.rule)], page_desc(&r), rows)
}

/// 添加规则的页：Kind、Rule，跟着打的字的 Saves as 与 Means，Add rule。
pub fn new_rule(ctx: &Ctx<'_>) -> View {
    let kind = behavior_of(&ctx.field(NEW_KIND, "allow")).unwrap_or(RuleBehavior::Allow);
    let saved = ctx.field(NEW_RULE, "");
    let typed = ctx
        .draft(NEW_RULE)
        .map_or_else(|| saved.clone(), str::to_owned);
    let set = |k: &'static str| {
        move |v: &str| Act::SetForm {
            key: k.to_owned(),
            value: v.trim().to_owned(),
        }
    };
    let mut text = TextRow::new(NEW_RULE, "Rule", saved, "", Applies::Form, set(NEW_RULE));
    text.hint = RULE_HINT.to_owned();
    let converted = (!typed.trim().is_empty()).then(|| convert(&typed));
    text.stuck = ctx.draft(NEW_RULE).is_some() && matches!(converted, Some(Err(_)));
    let dash = || vec![Part::new("—", Ink::Line)];
    let (saves, means_parts) = match &converted {
        None => (dash(), dash()),
        Some(Ok(rule)) => (
            vec![Part::new(rule.clone(), Ink::Fg2)],
            means(&parse_rule(rule)),
        ),
        Some(Err(why)) => (vec![Part::new(format!("✗ {why}"), Ink::Err)], dash()),
    };
    let mut rows = vec![
        kind_row(NEW_KIND, kind, set(NEW_KIND)),
        Row::Text(text),
        Row::info("Saves as", saves),
        Row::info("Means", means_parts),
        Row::Note(String::new()),
    ];
    let add = match &converted {
        Some(Ok(rule)) => Act::Write(
            Write::new(
                SettingsChange::AddRule {
                    behavior: kind,
                    rule: rule.clone(),
                },
                Applies::Now,
            )
            .saying(words::added_said())
            .going_back()
            .selecting(local_id(kind, rule)),
        ),
        Some(Err(why)) => Act::Refuse((*why).to_owned()),
        None => Act::Refuse(NEEDS_RULE.to_owned()),
    };
    let off = !matches!(converted, Some(Ok(_)));
    let mut row = Row::action("nr.add", "Add rule", "", add);
    if let Row::Action { off: o, .. } = &mut row {
        *o = off;
    }
    rows.push(row);
    if converted.is_none() {
        rows.push(Row::Note(NEEDS_RULE.to_owned()));
    }
    View::new(vec!["New rule".to_owned()], NEW_DESC, rows)
}

/// 添加页的表单是新的（再打开时是空的）。
pub fn clear_form(form: &mut std::collections::BTreeMap<String, String>) {
    form.remove(NEW_KIND);
    form.remove(NEW_RULE);
}
