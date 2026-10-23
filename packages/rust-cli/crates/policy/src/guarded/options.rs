// 移植自 apps/zcode-cli/packages/core/src/tool/handlers/guarded/options.ts 与 wrappers.ts（修复线 F20）。
//! 有限的选项文法：先吃掉已声明 arity 的参数值，未知选项不猜 arity；`env` / `time` / `sudo` 包装的剥离。

use super::word::{Word, basename};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arity {
    Flag,
    Value,
    Optional,
}

/// 选项表：三组用空白分隔的选项名。同名时后面的组优先（Node `Object.fromEntries` 的覆盖顺序）。
pub(crate) struct Table {
    pub flags: &'static str,
    pub values: &'static str,
    pub optional: &'static str,
}

impl Table {
    pub(crate) const fn flags(flags: &'static str) -> Self {
        Self {
            flags,
            values: "",
            optional: "",
        }
    }

    fn arity(&self, name: &str) -> Option<Arity> {
        let has = |group: &str| group.split_whitespace().any(|item| item == name);
        if has(self.optional) {
            Some(Arity::Optional)
        } else if has(self.values) {
            Some(Arity::Value)
        } else if has(self.flags) {
            Some(Arity::Flag)
        } else {
            None
        }
    }
}

/// 扫描策略（Node `OptionScanPolicy`）：包装与前置参数在第一个操作数处停、未知选项整体放弃；目标命令自身允许
/// 选项与操作数交错，未知选项只记 unsupported。
#[derive(Clone, Copy)]
pub(crate) struct Scan {
    pub stop_at_operand: bool,
    pub abort_on_unknown: bool,
}

pub(crate) const WRAPPER_SCAN: Scan = Scan {
    stop_at_operand: true,
    abort_on_unknown: true,
};
pub(crate) const COMMAND_SCAN: Scan = Scan {
    stop_at_operand: false,
    abort_on_unknown: false,
};

#[derive(Debug, Default)]
pub(crate) struct Parsed {
    pub flags: Vec<String>,
    pub operands: Vec<Word>,
    pub unsupported: bool,
}

/// Node `parseOptions`。
pub(crate) fn parse(words: &[Word], table: &Table, scan: Scan) -> Parsed {
    let mut out = Parsed::default();
    let mut ended = false;
    let mut i = 0;
    while i < words.len() {
        let word = &words[i];
        if !ended && !word.dynamic && word.value == "--" {
            ended = true;
            i += 1;
            continue;
        }
        if ended || word.dynamic || !word.value.starts_with('-') || word.value == "-" {
            out.operands.push(word.clone());
            if scan.stop_at_operand {
                out.operands.extend_from_slice(&words[i + 1..]);
                break;
            }
            i += 1;
            continue;
        }
        let long = word.value.starts_with("--");
        let equals = word.value.find('=');
        let names: Vec<String> = if long {
            vec![equals.map_or(word.value.clone(), |at| word.value[..at].to_owned())]
        } else {
            word.value
                .chars()
                .skip(1)
                .map(|c| format!("-{c}"))
                .collect()
        };
        for (n, name) in names.iter().enumerate() {
            let arity = table.arity(name);
            let attached = if long {
                equals.is_some()
            } else {
                n < names.len() - 1
            };
            // Node 2026-09-17 的修正：未知选项不丢掉同一个词里已识别的 -r / -f；abort 只给包装与前置参数。
            if arity.is_none() || (arity == Some(Arity::Flag) && long && attached) {
                out.unsupported = true;
                if scan.abort_on_unknown {
                    return out;
                }
                continue;
            }
            out.flags.push(name.clone());
            if arity == Some(Arity::Value) && !attached {
                i += 1;
                if words.get(i).is_none() {
                    out.unsupported = true;
                }
            }
            if arity != Some(Arity::Flag) {
                break;
            }
        }
        i += 1;
    }
    out
}

/// Node `enabled`：按出现顺序，后面的选项盖过前面的。
pub(crate) fn enabled(flags: &[String], yes: &[&str], no: &[&str]) -> bool {
    let mut result = false;
    for flag in flags {
        if yes.contains(&flag.as_str()) {
            result = true;
        }
        if no.contains(&flag.as_str()) {
            result = false;
        }
    }
    result
}

/// 一条命令的匹配结果（Node `RuleMatch`：规则名 / false / undefined）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hit {
    Rule(&'static str),
    /// 已支持、没有命中。
    No,
    /// 超出当前的命令或选项文法。
    Unknown,
}

/// Node `settleRule`：命中优先；否则遇到过未知选项是 Unknown，否则是 No。
pub(crate) fn settle(rule: Option<&'static str>, parsed: &Parsed) -> Hit {
    match rule {
        Some(rule) => Hit::Rule(rule),
        None if parsed.unsupported => Hit::Unknown,
        None => Hit::No,
    }
}

const ENV: Table = Table {
    flags: "-i --ignore-environment -0 --null -v --debug",
    values: "-u --unset -C --chdir",
    optional: "",
};
const TIME: Table = Table {
    flags: "-a --append -p --portability -v --verbose -q --quiet",
    values: "-o --output -f --format",
    optional: "",
};
const SUDO: Table = Table {
    flags: "-A --askpass -b --background -E --preserve-env -H --set-home -K --remove-timestamp -k --reset-timestamp -n --non-interactive -S --stdin",
    values: "-u --user -g --group -h --host -p --prompt -C --close-from -T --command-timeout -D --chdir -R --chroot",
    optional: "",
};

/// `NAME=value` 形式的词（`^[A-Za-z_][A-Za-z0-9_]*=`）。
fn assignment(value: &str) -> bool {
    let Some((name, _)) = value.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Node `unwrapCommand`：剥掉 `env` / `time` / `sudo`；包装的未知选项让真实命令的起点猜不出，整体放弃（None）。
pub(crate) fn unwrap(input: &[Word], windows: bool) -> Option<Vec<Word>> {
    let mut words = input.to_vec();
    while !words.is_empty() {
        let name = basename(&words[0], windows)?;
        let table = match name.as_str() {
            "env" => &ENV,
            "time" => &TIME,
            "sudo" => &SUDO,
            _ => return Some(words),
        };
        let parsed = parse(&words[1..], table, WRAPPER_SCAN);
        if parsed.unsupported {
            return None;
        }
        words = parsed.operands;
        if name == "env" || name == "sudo" {
            let skip = words.iter().take_while(|w| assignment(&w.value)).count();
            words.drain(..skip);
        }
    }
    None
}
