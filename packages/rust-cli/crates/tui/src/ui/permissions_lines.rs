//! `/permissions` 的排版与文字（docs/specs/rust-cli/tui-manage.md §1.2）：英文照原文（产品名换成
//! ZCode），中文另配；规则文字与来源路径不翻译。
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use rust_cli_kernel_api::permission::{PermissionRule, RuleBehavior, RuleSource};

use super::i18n::Lang;
use super::layout;
use super::palette::Palette;
use super::permissions::{
    PermissionsView, Stage, TABS, Tab, behavior_name, deletable, rule_text, source_name,
};
use super::text::{truncate_line, wrap_text};

fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

fn tab_name(tab: Tab, lang: Lang) -> &'static str {
    match (lang, tab) {
        (Lang::En, Tab::Allow) => "Allow",
        (Lang::En, Tab::Ask) => "Ask",
        (Lang::En, Tab::Deny) => "Deny",
        (Lang::En, Tab::Workspace) => "Workspace",
        (Lang::Zh, Tab::Allow) => "允许",
        (Lang::Zh, Tab::Ask) => "询问",
        (Lang::Zh, Tab::Deny) => "拒绝",
        (Lang::Zh, Tab::Workspace) => "工作区",
    }
}

/// 每个标签页的说明。
fn tab_note(tab: Tab, lang: Lang) -> &'static str {
    match (lang, tab) {
        (Lang::En, Tab::Allow) => "ZCode won't ask before using allowed tools.",
        (Lang::En, Tab::Ask) => "ZCode will always ask for confirmation before using these tools.",
        (Lang::En, Tab::Deny) => "ZCode will always reject requests to use denied tools.",
        (Lang::En, Tab::Workspace) => {
            "ZCode can read files in the workspace, and make edits when auto-accept edits is on."
        }
        (Lang::Zh, Tab::Allow) => "ZCode 使用允许的工具前不再询问。",
        (Lang::Zh, Tab::Ask) => "ZCode 使用这些工具前总是先确认。",
        (Lang::Zh, Tab::Deny) => "ZCode 总是拒绝使用被拒绝的工具。",
        (Lang::Zh, Tab::Workspace) => "ZCode 可以读取工作区里的文件；打开自动接受编辑时可以修改。",
    }
}

/// 规则说明：（前缀，强调部分，后缀）；没有说明时为 None。
fn description(rule: &PermissionRule, lang: Lang) -> Option<(&'static str, String, &'static str)> {
    let zh = lang == Lang::Zh;
    if rule.tool == "Bash" {
        return Some(match rule.content.as_deref() {
            None | Some("") => (
                if zh {
                    "任意 Bash 命令"
                } else {
                    "Any Bash command"
                },
                String::new(),
                "",
            ),
            Some(content) if content.ends_with(":*") || content.ends_with(" *") => (
                if zh {
                    "以此开头的任意 Bash 命令："
                } else {
                    "Any Bash command starting with "
                },
                content[..content.len() - ":*".len()].to_owned(),
                "",
            ),
            Some(content) => (
                if zh {
                    "Bash 命令 "
                } else {
                    "The Bash command "
                },
                content.to_owned(),
                "",
            ),
        });
    }
    if rule.content.as_deref().is_none_or(str::is_empty) {
        return Some(if zh {
            ("任意使用 ", rule.tool.clone(), " 工具")
        } else {
            ("Any use of the ", rule.tool.clone(), " tool")
        });
    }
    None
}

fn rule_block(rule: &PermissionRule, lang: Lang, width: usize) -> Vec<Line<'static>> {
    let text = rust_cli_runtime::format_permission_rule(rule);
    let mut lines = vec![truncate_line(
        Line::from(Span::styled(format!("    {text}"), bold())),
        width,
    )];
    if let Some((prefix, emphasis, suffix)) = description(rule, lang) {
        lines.push(truncate_line(
            Line::from(vec![
                Span::styled(format!("    {prefix}"), dim()),
                Span::styled(emphasis, dim().add_modifier(Modifier::BOLD)),
                Span::styled(suffix, dim()),
            ]),
            width,
        ));
    }
    lines
}

/// 底部的按键提示（`{键}` 模板，按键提示组件画，tui.md §7.4）。
fn hint(text: &str, palette: &Palette) -> Line<'static> {
    super::key_hint::line(layout::INDENT, text, palette)
}

fn wrapped(text: &str, style: Style, width: usize) -> Vec<Line<'static>> {
    wrap_text(
        text,
        style,
        width.saturating_sub(layout::INDENT),
        &[Span::raw("  ")],
        &[Span::raw("  ")],
    )
}

/// 页面的行（替换输入区）；`height` 是可用高度，列表只显示焦点附近的条目。
pub fn lines(
    view: &PermissionsView,
    lang: Lang,
    palette: &Palette,
    width: usize,
    height: usize,
) -> Vec<Line<'static>> {
    let zh = lang == Lang::Zh;
    let accent = palette.accent;
    let top =
        |color: Color| Line::from(Span::styled("─".repeat(width), Style::default().fg(color)));
    match &view.stage {
        Stage::Input(text) => {
            let behavior = view.tab.behavior().map_or("allow", behavior_name);
            let mut lines = vec![top(accent), title(&add_title(lang, behavior))];
            lines.push(Line::default());
            let note = if zh {
                "权限规则是工具名，后面可以跟括号里的限定。"
            } else {
                "Permission rules are a tool name, optionally followed by a specifier in parentheses."
            };
            lines.extend(wrapped(note, Style::default(), width));
            lines.push(Line::from(vec![
                Span::raw(if zh { "  例如 " } else { "  e.g., " }),
                Span::styled("WebFetch", bold()),
                Span::raw(if zh { " 或 " } else { " or " }),
                Span::styled("Bash(ls *)", bold()),
            ]));
            lines.push(Line::default());
            let shown = if text.is_empty() {
                Span::styled(
                    if zh {
                        "输入权限规则…"
                    } else {
                        "Enter permission rule…"
                    },
                    dim(),
                )
            } else {
                Span::raw(format!("{text}█"))
            };
            lines.push(truncate_line(
                Line::from(vec![
                    Span::styled("  > ", Style::default().fg(accent)),
                    shown,
                ]),
                width,
            ));
            lines.push(Line::default());
            lines.push(hint(
                if zh {
                    "{enter} 提交 · {esc} 取消"
                } else {
                    "{Enter} to submit · {Esc} to cancel"
                },
                palette,
            ));
            lines
        }
        Stage::Destination(text) => {
            let behavior = view.tab.behavior().map_or("allow", behavior_name);
            let rule = rust_cli_runtime::parse_permission_rule(text);
            let mut lines = vec![
                top(accent),
                title(&add_title(lang, behavior)),
                Line::default(),
            ];
            lines.extend(rule_block(&rule, lang, width));
            lines.push(Line::default());
            lines.push(Line::from(Span::raw(if zh {
                "  这条规则保存到哪里？"
            } else {
                "  Where should this rule be saved?"
            })));
            let (label, note) = if zh {
                ("本项目设置（本机）", "保存在本项目的本机设置里")
            } else {
                (
                    "Project settings (local)",
                    "Saved in this project's local settings",
                )
            };
            // 选择列表组件（`› ` 加底色条，tui.md §7.4，修复线 F26）。
            lines.push(super::pick::paint(
                truncate_line(
                    Line::from(vec![
                        Span::raw("  "),
                        super::pick::pointer(true),
                        Span::raw(format!("1. {label}")),
                        Span::styled(format!("   {note}"), palette.secondary()),
                    ]),
                    width,
                ),
                true,
                palette,
                width,
            ));
            lines.push(Line::default());
            lines.push(hint(
                if zh {
                    "{enter} 确认 · {esc} 取消"
                } else {
                    "{Enter} to confirm · {Esc} to cancel"
                },
                palette,
            ));
            lines
        }
        Stage::Details { rule, yes } => details(rule, *yes, lang, palette, width),
        Stage::List | Stage::Saving => list(view, lang, palette, width, height),
    }
}

fn title(text: &str) -> Line<'static> {
    Line::from(Span::styled(format!("  {text}"), bold()))
}

fn add_title(lang: Lang, behavior: &str) -> String {
    match lang {
        Lang::En => format!("Add {behavior} permission rule"),
        Lang::Zh => format!("添加{}权限规则", behavior_zh(behavior)),
    }
}

fn behavior_zh(behavior: &str) -> &'static str {
    match behavior {
        "ask" => "询问",
        "deny" => "拒绝",
        _ => "允许",
    }
}

fn details(
    rule: &rust_cli_kernel_api::permission::SourcedRule,
    yes: bool,
    lang: Lang,
    palette: &Palette,
    width: usize,
) -> Vec<Line<'static>> {
    let zh = lang == Lang::Zh;
    let accent = palette.accent;
    let from = Line::from(Span::styled(
        if zh {
            format!("    来自 {}", source_name(rule.source))
        } else {
            format!("    From {}", source_name(rule.source))
        },
        dim(),
    ));
    if !deletable(rule.source) {
        let file = if rule.source == RuleSource::UserSettings {
            "user"
        } else {
            "project"
        };
        let mut lines = vec![
            Line::from(Span::styled("─".repeat(width), Style::default().fg(accent))),
            title(if zh { "规则详情" } else { "Rule details" }),
            Line::default(),
        ];
        lines.extend(rule_block(&rule.rule, lang, width));
        lines.push(from);
        lines.push(Line::default());
        let note = if zh {
            format!("这条规则来自只读来源（{file} 配置文件），不能在这里修改。")
        } else {
            format!(
                "This rule comes from a read-only source (the {file} config file) and cannot be modified here."
            )
        };
        lines.extend(wrapped(
            &note,
            Style::default().add_modifier(Modifier::ITALIC),
            width,
        ));
        lines.push(Line::default());
        lines.push(hint(
            if zh {
                "{esc} 返回"
            } else {
                "{Esc} to cancel"
            },
            palette,
        ));
        return lines;
    }
    let red = Style::default().fg(Color::Red);
    let kind = match (lang, rule.behavior) {
        (Lang::En, RuleBehavior::Allow) => "allowed",
        (Lang::En, RuleBehavior::Deny) => "denied",
        (Lang::En, RuleBehavior::Ask) => "ask",
        (Lang::Zh, RuleBehavior::Allow) => "允许",
        (Lang::Zh, RuleBehavior::Deny) => "拒绝",
        (Lang::Zh, RuleBehavior::Ask) => "询问",
    };
    let heading = if zh {
        format!("  删除{kind}的工具？")
    } else {
        format!("  Delete {kind} tool?")
    };
    let mut lines = vec![
        Line::from(Span::styled("─".repeat(width), red)),
        Line::from(Span::styled(heading, red.add_modifier(Modifier::BOLD))),
        Line::default(),
    ];
    lines.extend(rule_block(&rule.rule, lang, width));
    lines.push(from);
    lines.push(Line::default());
    lines.push(Line::from(Span::raw(if zh {
        "  确定删除这条权限规则吗？"
    } else {
        "  Are you sure you want to delete this permission rule?"
    })));
    let option = |label: &str, focused: bool| {
        super::pick::paint(
            Line::from(vec![
                Span::raw("  "),
                super::pick::pointer(focused),
                Span::raw(label.to_owned()),
            ]),
            focused,
            palette,
            width,
        )
    };
    let (yes_label, no_label) = if zh { ("是", "否") } else { ("Yes", "No") };
    lines.push(option(yes_label, yes));
    lines.push(option(no_label, !yes));
    lines.push(Line::default());
    lines.push(hint(
        if zh {
            "{enter} 确认 · {esc} 取消"
        } else {
            "{Enter} to confirm · {Esc} to cancel"
        },
        palette,
    ));
    lines
}

fn list(
    view: &PermissionsView,
    lang: Lang,
    palette: &Palette,
    width: usize,
    height: usize,
) -> Vec<Line<'static>> {
    let zh = lang == Lang::Zh;
    let accent = palette.accent;
    let mut tabs = vec![Span::styled(
        if zh { "  权限：" } else { "  Permissions:" },
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    )];
    for tab in TABS {
        let name = format!("  {}  ", tab_name(tab, lang));
        tabs.push(if tab == view.tab {
            Span::styled(
                name,
                Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            )
        } else {
            Span::styled(name, dim())
        });
    }
    let mut lines = vec![
        Line::from(Span::styled("─".repeat(width), Style::default().fg(accent))),
        truncate_line(Line::from(tabs), width),
        Line::default(),
    ];
    lines.extend(wrapped(tab_note(view.tab, lang), Style::default(), width));
    lines.push(Line::default());
    if view.tab == Tab::Workspace {
        let label = if zh {
            "（原始工作目录）"
        } else {
            " (Original working directory)"
        };
        lines.push(truncate_line(
            Line::from(vec![
                Span::raw(format!("  - {}", view.workspace)),
                Span::styled(label, dim()),
            ]),
            width,
        ));
        lines.push(Line::default());
        lines.push(hint(
            if zh {
                "{←/→} 切换 · {esc} 取消"
            } else {
                "{←/→} to switch · {Esc} to cancel"
            },
            palette,
        ));
        return lines;
    }
    if let Some(filter) = &view.filter {
        lines.push(Line::from(vec![
            Span::styled("  ⌕ ", dim()),
            Span::raw(filter.clone()),
        ]));
    }
    let mut items: Vec<String> = Vec::new();
    if view.has_add() {
        items.push(
            if zh {
                "添加新规则…"
            } else {
                "Add a new rule…"
            }
            .to_owned(),
        );
    }
    items.extend(view.tab_rules().into_iter().map(rule_text));
    if items.is_empty() {
        lines.push(hint(if zh { "没有匹配" } else { "No matches" }, palette));
    }
    let room = height
        .saturating_sub(lines.len() + layout::FOOTER_ROWS)
        .max(layout::MIN_LIST_ROWS);
    let start = view.selected.saturating_sub(room.saturating_sub(1));
    for (index, item) in items.iter().enumerate().skip(start).take(room) {
        let chosen = index == view.selected;
        let line = Line::from(vec![
            Span::raw("  "),
            super::pick::pointer(chosen),
            Span::raw(item.clone()),
        ]);
        lines.push(super::pick::paint(
            truncate_line(line, width),
            chosen,
            palette,
            width,
        ));
    }
    lines.push(Line::default());
    lines.push(hint(
        match (lang, view.filter.is_some(), &view.stage) {
            (Lang::En, _, Stage::Saving) => "Saving…",
            (Lang::Zh, _, Stage::Saving) => "保存中…",
            (Lang::En, true, _) => "Type to filter · {Enter/↓} to select · {Esc} to clear",
            (Lang::Zh, true, _) => "输入以过滤 · {enter/↓} 选择 · {esc} 清除",
            (Lang::En, false, _) => {
                "{↑/↓} to navigate · {Enter} to select · {←/→} to switch · {Esc} to cancel"
            }
            (Lang::Zh, false, _) => "{↑/↓} 移动 · {enter} 选择 · {←/→} 切换 · {esc} 取消",
        },
        palette,
    ));
    lines
}

pub(crate) fn added(lang: Lang, behavior: &str, rule: &str) -> String {
    match lang {
        Lang::En => format!("Added {behavior} rule {rule}"),
        Lang::Zh => format!("已添加{}规则 {rule}", behavior_zh(behavior)),
    }
}

pub(crate) fn deleted(lang: Lang, behavior: &str, rule: &str) -> String {
    match lang {
        Lang::En => format!("Deleted {behavior} rule {rule}"),
        Lang::Zh => format!("已删除{}规则 {rule}", behavior_zh(behavior)),
    }
}

pub(crate) fn add_failed(lang: Lang, rule: &str, reason: &str) -> String {
    match lang {
        Lang::En => format!("Could not add {rule}: {reason}"),
        Lang::Zh => format!("无法添加 {rule}：{reason}"),
    }
}

pub(crate) fn delete_failed(lang: Lang, rule: &str) -> String {
    match lang {
        Lang::En => format!("Could not delete {rule} — the rule is unchanged"),
        Lang::Zh => format!("无法删除 {rule}，规则没有变化"),
    }
}

pub(crate) fn dismissed(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Permissions dialog dismissed",
        Lang::Zh => "已关闭权限页面",
    }
}
