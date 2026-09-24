//! `util.parseArgs({strict: true, allowPositionals: true})` over Node's global
//! option table, and the greedy `--disallowed-tools` pre-extraction
//! (`cli/src/arguments.ts`).
use crate::args::{ArgError, parse_error};
use std::collections::BTreeMap;

/// `(long, short, takes a value, repeatable)`; Node's global option table plus
/// the Rust-only `--data-dir` and `--config`.
const OPTIONS: &[(&str, Option<char>, bool, bool)] = &[
    ("help", Some('h'), false, false),
    ("json", None, false, false),
    ("output-format", None, true, false),
    ("no-color", None, false, false),
    ("no-browser", None, false, false),
    ("browser-use", None, true, false),
    ("browser-executable", None, true, false),
    ("prompt", Some('p'), true, false),
    ("memory-bench", None, false, false),
    ("attach", None, true, true),
    ("cwd", None, true, false),
    ("locale", None, true, false),
    ("resume", None, true, false),
    ("target", None, true, false),
    ("target-replace", None, false, false),
    ("continue", Some('c'), false, false),
    ("force", Some('f'), false, false),
    ("force-mcs", None, false, false),
    ("mode", None, true, false),
    ("verbose", None, false, false),
    ("version", Some('v'), false, false),
    ("prepare-storage", None, false, false),
    ("stdio", None, false, false),
    ("surface", None, true, false),
    ("all", Some('a'), false, false),
    ("available", None, false, false),
    ("keep-data", None, false, false),
    ("scope", Some('s'), true, false),
    ("sparse", None, true, true),
    ("data-dir", None, true, false),
    ("config", None, true, false),
];

#[derive(Default)]
pub(crate) struct Values {
    pub(crate) strings: BTreeMap<&'static str, Vec<String>>,
    flags: BTreeMap<&'static str, bool>,
}

impl Values {
    pub(crate) fn string(&self, name: &str) -> Option<&str> {
        self.strings
            .get(name)
            .and_then(|v| v.last())
            .map(String::as_str)
    }
    pub(crate) fn flag(&self, name: &str) -> bool {
        self.flags.get(name).copied().unwrap_or(false)
    }
}

/// Node `isCliOptionToken`.
fn option_token(value: &str) -> bool {
    value.starts_with('-')
}

/// Node `extractDisallowedToolsArgs`: the flag greedily takes every following
/// non-option argument.
pub(crate) fn extract_disallowed(argv: &[String]) -> Result<(Vec<String>, Vec<String>), ArgError> {
    const FLAGS: [&str; 2] = ["--disallowedTools", "--disallowed-tools"];
    let (mut args, mut values) = (vec![], vec![]);
    let mut index = 0;
    while index < argv.len() {
        let arg = &argv[index];
        index += 1;
        if let Some((flag, value)) = arg.split_once('=')
            && FLAGS.contains(&flag)
        {
            values.push(value.to_owned());
            continue;
        }
        if !FLAGS.contains(&arg.as_str()) {
            args.push(arg.clone());
            continue;
        }
        let start = index;
        while index < argv.len() && !option_token(&argv[index]) {
            values.push(argv[index].clone());
            index += 1;
        }
        if index == start {
            return parse_error(format!("{arg} requires at least one tool."));
        }
    }
    Ok((args, tool_rules(&values)))
}

/// Node `normalizeCliToolRuleList`: split on commas and spaces outside
/// parentheses, drop duplicates, `web_search` is `WebSearch`.
fn tool_rules(values: &[String]) -> Vec<String> {
    let mut rules: Vec<String> = vec![];
    for value in values {
        let (mut current, mut nested) = (String::new(), false);
        let mut parts = vec![];
        for c in value.chars() {
            match c {
                '(' | ')' => {
                    nested = c == '(';
                    current.push(c);
                }
                ',' | ' ' if !nested => parts.push(std::mem::take(&mut current)),
                _ => current.push(c),
            }
        }
        parts.push(current);
        for part in parts {
            let part = part.trim();
            let rule = match part.strip_prefix("web_search") {
                Some(rest) if rest.is_empty() || rest.starts_with('(') => {
                    format!("WebSearch{rest}")
                }
                _ => part.to_owned(),
            };
            if !rule.is_empty() && !rules.contains(&rule) {
                rules.push(rule);
            }
        }
    }
    rules
}

fn spec(name: &str) -> Option<&'static (&'static str, Option<char>, bool, bool)> {
    OPTIONS.iter().find(|o| o.0 == name)
}

fn short_spec(c: char) -> Option<&'static (&'static str, Option<char>, bool, bool)> {
    OPTIONS.iter().find(|o| o.1 == Some(c))
}

fn unknown<T>(raw: &str) -> Result<T, ArgError> {
    parse_error(format!(
        "Unknown option '{raw}'. To specify a positional argument starting with a '-', place it at the end of the command after '--', as in '-- {}",
        serde_json::Value::String(raw.into())
    ))
}

/// Stores one option token with Node's strict checks (`checkOptionUsage`,
/// `checkOptionLikeValue`). `inline` marks `--name=value` / `-pvalue`.
fn store(
    values: &mut Values,
    option: &'static (&'static str, Option<char>, bool, bool),
    raw: &str,
    value: Option<String>,
    inline: bool,
) -> Result<(), ArgError> {
    let (long, short, takes, _) = *option;
    let named = match short {
        Some(short) => format!("-{short}, --{long}"),
        None => format!("--{long}"),
    };
    if !takes {
        if value.is_some() {
            return parse_error(format!("Option '{named}' does not take an argument"));
        }
        values.flags.insert(long, true);
        return Ok(());
    }
    let Some(value) = value else {
        return parse_error(format!("Option '{named} <value>' argument missing"));
    };
    if !inline && value.len() > 1 && value.starts_with('-') {
        let example = if raw.starts_with("--") {
            format!("'{raw}=-XYZ'")
        } else {
            format!("'--{long}=-XYZ' or '{raw}-XYZ'")
        };
        return parse_error(format!(
            "Option '{raw}' argument is ambiguous.\nDid you forget to specify the option argument for '{raw}'?\nTo specify an option argument starting with a dash use {example}."
        ));
    }
    values.strings.entry(long).or_default().push(value);
    Ok(())
}

/// `util.parseArgs({strict: true, allowPositionals: true})`.
pub(crate) fn parse_values(args: Vec<String>) -> Result<(Values, Vec<String>), ArgError> {
    let mut values = Values::default();
    let mut positionals = vec![];
    let mut queue: std::collections::VecDeque<String> = args.into();
    while let Some(arg) = queue.pop_front() {
        if arg == "--" {
            positionals.extend(queue.drain(..));
            break;
        }
        let chars: Vec<char> = arg.chars().collect();
        if chars.len() == 2 && chars[0] == '-' && chars[1] != '-' {
            let Some(option) = short_spec(chars[1]) else {
                return unknown(&arg);
            };
            let value = if option.2 { queue.pop_front() } else { None };
            store(&mut values, option, &arg, value, false)?;
        } else if chars.len() > 2 && chars[0] == '-' && chars[1] != '-' {
            match short_spec(chars[1]).filter(|o| o.2) {
                // -pVALUE
                Some(option) => {
                    let raw = format!("-{}", chars[1]);
                    let value: String = chars[2..].iter().collect();
                    store(&mut values, option, &raw, Some(value), true)?;
                }
                // -abc：展开为 -a -b -c，中途的取值选项吞下剩余字符。
                None => {
                    let mut expanded = vec![];
                    for (index, c) in chars.iter().enumerate().skip(1) {
                        let takes = short_spec(*c).is_some_and(|o| o.2);
                        if !takes || index == chars.len() - 1 {
                            expanded.push(format!("-{c}"));
                        } else {
                            expanded
                                .push(format!("-{}", chars[index..].iter().collect::<String>()));
                            break;
                        }
                    }
                    for item in expanded.into_iter().rev() {
                        queue.push_front(item);
                    }
                }
            }
        } else if let Some(body) = arg.strip_prefix("--").filter(|b| !b.is_empty()) {
            let (name, inline) = match body.split_once('=') {
                Some((name, value)) => (name, Some(value.to_owned())),
                None => (body, None),
            };
            let raw = format!("--{name}");
            let Some(option) = spec(name) else {
                return unknown(&raw);
            };
            let is_inline = inline.is_some();
            let value = match inline {
                Some(value) => Some(value),
                None if option.2 => queue.pop_front(),
                None => None,
            };
            store(&mut values, option, &raw, value, is_inline)?;
        } else {
            positionals.push(arg);
        }
    }
    Ok((values, positionals))
}
