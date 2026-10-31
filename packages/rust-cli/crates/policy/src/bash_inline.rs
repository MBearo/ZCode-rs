//! shell -c 脚本（shell-permissions.md §4.5.9，工作线 F85 追加，用户决定 2026-10-24）。
//!
//! `sh -c '<脚本>'` 的脚本是一个字符串参数，外层的检查看不到里面的命令。照对照实现的范围：脚本里有 rm 一类的词时把脚本
//! 当一条 Bash 命令再判一次；里面有被拒绝的命令、或脚本检查不了而又跑 rm 时，按危险删除一类询问（guarded 也问，yolo 由
//! 判决链放行）；里面是危险删除时按它询问。出处见 provenance/policy.md 的 F85。
//!
//! 修复（工作线 F85 追加）：以前 `Bash(rm:*)` 下 `sh -c 'rm -f x'` 只是“需要批准”（其他类），guarded 下执行。

use std::sync::OnceLock;

use regex::Regex;

use crate::check::{AskKind, Check, Inputs};
use crate::consts::SHORT_OPTION_LEN;
use crate::parse::Invocation;
use crate::removal::{basename, strip_runners};
use crate::shell_argv;

/// 认作 shell 的命令名。
const SHELLS: [&str; 9] = [
    "bash", "sh", "zsh", "dash", "ksh", "mksh", "ash", "hush", "rbash",
];
/// 里面那次判决最多嵌套的层数；再深按“检查不了”。
const MAX_DEPTH: usize = 2;
/// 脚本超过这个长度按“检查不了”。
const MAX_SCRIPT: usize = 2000;
/// 位置参数的值不是字面值时的占位。
const VALUE: &str = "__INLINE_SHELL_VALUE__";
/// 让命令名之前的词不算命令名的包装与关键字。
const WRAPPERS: [&str; 35] = [
    "sudo", "doas", "exec", "command", "env", "nice", "nohup", "busybox", "time", "timeout",
    "stdbuf", "setsid", "ionice", "pkexec", "runuser", "eval", "xargs", "do", "then", "else",
    "elif", "if", "while", "until", "!", "{", "coproc", "builtin", "noglob", "toybox", "trap",
    "watch", "flock", "chroot", "script",
];

const DENIED_INSIDE: &str = "A permission rule denies a command inside this shell -c script, so ZCode could not check it for dangerous removals. Approving runs the whole script, including that command.";
const UNCHECKED: &str = "This command passes a shell -c script that runs rm, and ZCode could not check the script for dangerous removals. Approve only if you have read the script.";

/// 一个 shell 命令的脚本：原文，与能拿去判决的文字（检查不了时 None）。
struct Script {
    text: String,
    judged: Option<String>,
}

struct Patterns {
    known_long: Regex,
    ifs_read: Regex,
    ifs: Regex,
    alias: Regex,
    zsh: Regex,
    positional: Regex,
    literal: Regex,
    unsupported: Regex,
    brace: Regex,
    comma_range: Regex,
    rm_word: Regex,
    command_dollar: Regex,
    quoted: Regex,
    dup: Regex,
    redirect_ops: Regex,
    dequote: Regex,
    split: Regex,
    redirect_token: Regex,
    redirect_only: Regex,
    assignment: Regex,
    rm_name: Regex,
    rm_list: Regex,
    rm_assignment: Regex,
    exec_flag: Regex,
    words: Regex,
    brace_parts: Regex,
    rm_tail: Regex,
    dollar_brace: Regex,
    redirect_tail: Regex,
}

fn patterns() -> Option<&'static Patterns> {
    static PATTERNS: OnceLock<Option<Patterns>> = OnceLock::new();
    PATTERNS
        .get_or_init(|| {
            let re = |source: &str| Regex::new(source).ok();
            Some(Patterns {
                known_long: re(
                    r"^--?(?:login|posix|noprofile|norc|noediting|verbose|restricted|debugger|dump-(?:po-)?strings|wordexp|pretty-print|help|version|(rcfile|init-file))$",
                )?,
                ifs_read: re(
                    r#"(?:^|[;&|\n({]\s*)(?:(?:do|then|else|elif|if|while|until)\s+)?IFS=(?:'[^']*'|"[^"]*"|[^\s;&|'"\\])*[ \t]+read\b"#,
                )?,
                ifs: re(r"\bIFS\b")?,
                alias: re(r"\balias\b")?,
                zsh: re(
                    r"(?:^|[\s;&|(){}])=[^=\s]|\$\{?\w+\}?(?::[htrelquQxsaAcP&]|\[)|\bargv\b|\$(?:[=~^]|\{[=~^(])|\(:",
                )?,
                positional: re(r"\$(?:\{[!#@*0-9]|[#@*0-9])")?,
                literal: re(r"^[\w./@%+=:,*?\[\]!^{} \t\n-]*$")?,
                unsupported: re(r"\b(?:function|alias|IFS|shift|source|eval|trap|set)\b|\w\s*\(\s*\)")?,
                brace: re(r"\{[^{}\s]*(?:,|\.\.)")?,
                comma_range: re(r",|\.\.")?,
                rm_word: re(r"(?i)(?:^|[^\w.-])rm(?:dir)?(?:$|[^\w.-])")?,
                command_dollar: re(r"(?:^|[;&|(){}\n`!])\s*(?:\w+\+?=\S*\s+)*\$")?,
                quoted: re(r#"\\[\s\S]|'[^']*'|"(?:[^"\\]|\\.)*""#)?,
                dup: re(r"\d*[<>]&(?:\d+-?|-)")?,
                redirect_ops: re(r"\d*[<>]&|&>>?|>\|")?,
                dequote: re(r#"\$['"]|['"\\]"#)?,
                split: re(r"[;&|()`{}\n!]|\$\(")?,
                redirect_token: re(r"^\d*[<>]")?,
                redirect_only: re(r"^\d*[<>]+$")?,
                assignment: re(r"^\w+\+?=")?,
                rm_name: re(r"(?i)^(?:\w*=)?rm(?:dir)?$")?,
                rm_list: re(r"(?i)(?:^|,)rm(?:dir)?(?:,|\$)")?,
                rm_assignment: re(r"(?i)^\w+=(?:\S*/)?rm(?:dir)?$")?,
                exec_flag: re(r"^-(?:exec(?:dir)?|ok(?:dir)?)$")?,
                words: re(r#"(?:[^\s'"\\]|\\[\s\S]|'[^']*'|"(?:[^"\\]|\\.)*")+"#)?,
                brace_parts: re(r"(?s)^([^{]*)\{([^{}]*)\}(.*)$")?,
                rm_tail: re(r"(?i)(?:^|/)rm(?:dir)?$")?,
                dollar_brace: re(r"\$\{[^{}]*\}")?,
                redirect_tail: re(r"(?s)\d*[<>].*$")?,
            })
        })
        .as_ref()
}

/// 命令名：去目录、去 `.exe`、小写、去版本后缀（`bash-5.2`）。
fn shell_name(word: &str) -> String {
    let base = basename(word);
    let base = base
        .strip_suffix(".exe")
        .or_else(|| base.strip_suffix(".EXE"))
        .unwrap_or(base)
        .to_lowercase();
    match base.rfind('-') {
        Some(at)
            if at > 0
                && base[at + 1..]
                    .split('.')
                    .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())) =>
        {
            base[..at].to_owned()
        }
        _ => base,
    }
}

/// 花括号展开会落到路径或 rm 上（对照实现把这种脚本算检查不了）。
fn risky_brace(word: &str, patterns: &Patterns) -> bool {
    let text = patterns.dollar_brace.replace_all(word, "$$X");
    let opens = text.matches('{').count();
    if opens == 0 || !patterns.comma_range.is_match(&text) {
        return false;
    }
    let Some(found) = patterns.brace_parts.captures(&text) else {
        return true;
    };
    if opens > 1 {
        return true;
    }
    let (head, inner, tail) = (&found[1], &found[2], &found[3]);
    if !patterns.comma_range.is_match(inner) {
        return false;
    }
    let head_risky = head
        .trim_start_matches(['"', '\''])
        .starts_with(['/', '~', '$'])
        || head.contains(['{', '}', ',', '\\', '\'', '"'])
        || head.contains("..");
    head_risky
        || tail.contains(['~', '{', '}', ',', '\\', '\'', '"'])
        || inner
            .split([','])
            .flat_map(|part| part.split(".."))
            .map(|part| part.replace(['"', '\'', '\\'], ""))
            .any(|part| {
                part.contains(['/', '~', '$', '*', '?', '['])
                    || part == "."
                    || part == ".."
                    || part.to_lowercase().contains("rm")
                    || (part.is_empty()
                        && (format!("{head}{tail}").contains('$')
                            || format!("{head}{tail}")
                                .trim_start_matches(['"', '\''])
                                .starts_with('/')))
            })
        || brace_runs_rm(word, patterns)
}

/// 花括号展开出来的某一项是 rm / rmdir。
fn brace_runs_rm(word: &str, patterns: &Patterns) -> bool {
    let plain = word.replace(['"', '\'', '\\'], "");
    patterns.brace_parts.captures(&plain).is_some_and(|found| {
        found[2]
            .split(',')
            .flat_map(|part| part.split(".."))
            .any(|part| {
                patterns
                    .rm_tail
                    .is_match(&format!("{}{part}{}", &found[1], &found[3]))
            })
    })
}

/// 位置参数换成 `-c` 脚本之后的参数；换不了时 None（检查不了）。
fn substitute(script: &str, values: &[String], patterns: &Patterns) -> Option<String> {
    if script.contains('\'') || patterns.unsupported.is_match(script) {
        return None;
    }
    let shown: Vec<&str> = values
        .iter()
        .map(|value| patterns.literal.is_match(value).then_some(value.as_str()))
        .collect::<Option<_>>()?;
    let bytes = script.as_bytes();
    let mut out = String::with_capacity(script.len());
    let mut index = 0;
    while index < bytes.len() {
        let rest = &script[index..];
        if rest.starts_with('\\') {
            let width = rest.chars().nth(1).map_or(1, |c| 1 + c.len_utf8());
            out.push_str(&rest[..width.min(rest.len())]);
            index += width.min(rest.len());
            continue;
        }
        if let Some(after) = rest
            .strip_prefix("\"$@\"")
            .or_else(|| rest.strip_prefix("\"${@}\""))
        {
            let quoted: Vec<String> = shown
                .iter()
                .skip(1)
                .map(|value| format!("\"{value}\""))
                .collect();
            out.push_str(&quoted.join(" "));
            index = script.len() - after.len();
            continue;
        }
        if !rest.starts_with('$') {
            let c = rest.chars().next().unwrap_or_default();
            out.push(c);
            index += c.len_utf8();
            continue;
        }
        let body = &rest[1..];
        let (name, width) = if let Some(inner) = body.strip_prefix('{') {
            let end = inner.find('}')?;
            (&inner[..end], end + "{}".len())
        } else {
            match body.chars().next() {
                Some(c @ ('@' | '*' | '#')) => (&body[..c.len_utf8()], c.len_utf8()),
                Some(c) if c.is_ascii_digit() => (&body[..1], 1),
                _ => {
                    out.push('$');
                    index += 1;
                    continue;
                }
            }
        };
        let value = match name {
            "#" => (shown.len().saturating_sub(1)).to_string(),
            "@" | "*" => {
                // 有第二个位置参数时 `$@` 不止一个词，不代入。
                if shown.get(2).is_some() {
                    return None;
                }
                shown.get(1).copied().unwrap_or_default().to_owned()
            }
            digits if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => {
                let at: usize = digits.parse().ok()?;
                shown.get(at).copied().unwrap_or_default().to_owned()
            }
            _ if body.starts_with('{') => return None,
            _ => {
                out.push('$');
                index += 1;
                continue;
            }
        };
        out.push_str(&value);
        index += 1 + width;
    }
    Some(out)
}

/// shell 命令的 `-c` 脚本（argv 已去掉包装）；不是 shell 或没有 `-c` 时 None。
fn script_of(argv: &[String], patterns: &Patterns) -> Option<Script> {
    let mut words = argv;
    let first = shell_name(words.first()?);
    if matches!(first.as_str(), "busybox" | "toybox")
        && words
            .get(1)
            .is_some_and(|word| SHELLS.contains(&shell_name(word).as_str()))
    {
        words = &words[1..];
    }
    let shell = shell_name(words.first()?);
    if !SHELLS.contains(&shell.as_str()) {
        return None;
    }
    let rest = &words[1..];
    let sh_family = matches!(shell.as_str(), "sh" | "bash" | "rbash");
    let (mut has_c, mut odd, mut at) = (false, false, 0);
    while at < rest.len() {
        let word = rest[at].as_str();
        if word == "-" || word == "--" {
            at += 1;
            break;
        }
        if word.chars().count() < SHORT_OPTION_LEN || !word.starts_with(['-', '+']) {
            break;
        }
        let known = sh_family
            .then(|| patterns.known_long.captures(word))
            .flatten();
        if let Some(found) = known {
            at += usize::from(found.get(1).is_some());
        } else if word.starts_with("--") {
            odd = true;
        } else {
            let flags = &word[1..];
            odd |= (sh_family
                && flags
                    .chars()
                    .any(|c| !"abcefhiklmnoprstuvxBCDEHIOPT".contains(c)))
                || (shell.ends_with("ksh")
                    && word
                        .char_indices()
                        .any(|(i, c)| matches!(c, 'o' | 'O') && i + 1 < word.len()));
            has_c |= word.starts_with('-') && word.contains('c');
            at += word.matches(['o', 'O']).count();
        }
        at += 1;
    }
    if odd {
        let any_c = rest.iter().any(|word| {
            word.strip_prefix('-').is_some_and(|flags| {
                flags
                    .chars()
                    .take_while(char::is_ascii_alphabetic)
                    .any(|c| c == 'c')
            })
        });
        return any_c.then(|| Script {
            text: rest.join(" "),
            judged: None,
        });
    }
    let script = rest.get(at)?;
    if !has_c {
        return None;
    }
    let unchecked = || Script {
        text: script.clone(),
        judged: None,
    };
    let without_read = patterns.ifs_read.replace_all(script, " read");
    if script.chars().count() > MAX_SCRIPT
        || patterns
            .words
            .find_iter(script)
            .any(|word| risky_brace(word.as_str(), patterns))
        || patterns.alias.is_match(script)
        || patterns.ifs.is_match(&without_read)
        || (shell == "zsh" && patterns.zsh.is_match(script))
    {
        return Some(unchecked());
    }
    if !patterns.positional.is_match(script) {
        return Some(Script {
            text: script.clone(),
            judged: Some(script.clone()),
        });
    }
    // 脚本之后是 `$0`，再之后是 `$1` 起的位置参数。
    let dollar_zero = at + 1;
    let mut values = vec![
        rest.get(dollar_zero)
            .cloned()
            .unwrap_or_else(|| words[0].clone()),
    ];
    values.extend(rest.iter().skip(dollar_zero + 1).cloned());
    Some(Script {
        text: script.clone(),
        judged: substitute(script, &values, patterns),
    })
}

/// 脚本值得再判一次：有 rm 一类的词、带 `,` / `..` 的花括号、命令位置上的展开，或 `$'…'`。
fn worth_checking(script: &str, raw: &str, patterns: &Patterns) -> bool {
    let plain = patterns
        .dequote
        .replace_all(script, |found: &regex::Captures<'_>| {
            if found[0].starts_with('$') {
                found[0][1..].to_owned()
            } else {
                String::new()
            }
        });
    let plain = plain.replace(['\'', '"', '\\'], "");
    patterns.brace.is_match(&plain)
        || patterns.rm_word.is_match(&plain)
        || patterns.command_dollar.is_match(&plain)
        || raw.contains("$'")
}

/// 把脚本按分隔符切成段、段切成词（引号里的分隔符不切；`mask` 为假时不看引号）。
fn segments(script: &str, mask: bool, patterns: &Patterns) -> Vec<Vec<String>> {
    let mut text = if mask {
        patterns
            .quoted
            .replace_all(script, |found: &regex::Captures<'_>| {
                let all = &found[0];
                if all.starts_with('\\') {
                    all.to_owned()
                } else {
                    all.chars()
                        .map(|c| {
                            if c.is_whitespace() || ";&|()`{}!".contains(c) {
                                '\0'
                            } else {
                                c
                            }
                        })
                        .collect()
                }
            })
            .into_owned()
    } else {
        script.to_owned()
    };
    // `2>&1` 一类后面是结尾或分隔符时换成空白。
    let mut kept = String::with_capacity(text.len());
    let mut cursor = 0;
    for found in patterns.dup.find_iter(&text) {
        let next = text[found.end()..].chars().next();
        if next.is_none_or(|c| c.is_whitespace() || ";&|()".contains(c)) {
            kept.push_str(&text[cursor..found.start()]);
            kept.push(' ');
            cursor = found.end();
        }
    }
    kept.push_str(&text[cursor..]);
    text = patterns.redirect_ops.replace_all(&kept, ">").into_owned();
    text = patterns
        .dequote
        .replace_all(&text, |found: &regex::Captures<'_>| {
            if found[0].starts_with('$') {
                found[0][1..].replace(['\'', '"'], "")
            } else {
                String::new()
            }
        })
        .into_owned();
    patterns
        .split
        .split(&text)
        .map(|segment| {
            let tokens: Vec<&str> = segment
                .split([' ', '\t'])
                .filter(|t| !t.is_empty())
                .collect();
            tokens
                .iter()
                .enumerate()
                .filter(|(index, token)| {
                    let redirect = patterns.redirect_token.is_match(token);
                    let after_redirect = index
                        .checked_sub(1)
                        .is_some_and(|before| patterns.redirect_only.is_match(tokens[before]));
                    let leading_assignment = patterns.assignment.is_match(token)
                        && tokens[..*index]
                            .iter()
                            .all(|t| patterns.assignment.is_match(t));
                    !(redirect || after_redirect || leading_assignment)
                })
                .map(|(_, token)| (*token).to_owned())
                .collect()
        })
        .collect()
}

/// 命令名（去目录、去 `.exe`）。
fn bare(word: &str) -> String {
    let base = basename(word);
    base.strip_suffix(".exe")
        .or_else(|| base.strip_suffix(".EXE"))
        .unwrap_or(base)
        .to_owned()
}

/// 脚本跑 rm / rmdir：在命令位置、在包装命令之后，或和 `-c` / `-exec` 一起出现。
fn runs_rm(script: &str, patterns: &Patterns) -> bool {
    let rm_like = |token: &str| {
        let head = patterns.redirect_tail.replace(token, "");
        patterns.rm_name.is_match(&bare(&head)) || patterns.rm_list.is_match(token)
    };
    let wrapper = |token: &str| WRAPPERS.contains(&bare(token).to_lowercase().as_str());
    let short_c = |token: &str| {
        token.len() > 1
            && token.starts_with('-')
            && token[1..].bytes().all(|b| b.is_ascii_alphabetic())
            && token.contains('c')
    };
    let mut all = segments(script, true, patterns);
    all.extend(segments(script, false, patterns));
    all.iter().any(|tokens| {
        let passed_on = tokens.iter().enumerate().any(|(index, token)| {
            rm_like(token)
                && tokens.iter().enumerate().any(|(other, word)| {
                    other != index
                        && (wrapper(word) || short_c(word) || patterns.exec_flag.is_match(word))
                })
        });
        let at_command = tokens.iter().enumerate().any(|(index, token)| {
            (rm_like(token) || token.starts_with('$'))
                && (index == 0 || wrapper(&tokens[index - 1]))
        });
        passed_on
            || at_command
            || tokens
                .iter()
                .any(|token| patterns.rm_assignment.is_match(token))
    }) || patterns
        .words
        .find_iter(script)
        .any(|word| brace_runs_rm(word.as_str(), patterns))
}

/// 一条简单命令与它前面（同一截里）的 cd 段的原文。
pub(crate) struct Candidate<'a> {
    pub invocation: &'a Invocation,
    pub cds: Vec<&'a str>,
}

/// 检查一组简单命令里的 shell -c 脚本；要询问时 Some。`joiner` 是接 cd 与脚本用的连接符，`depth` 是嵌套层数。
pub(crate) fn check(
    candidates: &[Candidate<'_>],
    joiner: &str,
    inputs: &Inputs<'_>,
    depth: usize,
) -> Option<Check> {
    let patterns = patterns()?;
    for candidate in candidates {
        let mut argv = candidate.invocation.argv.clone();
        if let Some(first) = argv.first_mut() {
            *first = bare(first);
        }
        let argv = shell_argv::strip_wrappers(strip_runners(&argv));
        let Some(script) = script_of(&argv, patterns) else {
            continue;
        };
        let target = script.judged.as_deref().unwrap_or(&script.text);
        if !worth_checking(target, &script.text, patterns) {
            continue;
        }
        let unchecked = script.judged.is_none() || depth >= MAX_DEPTH;
        let mut denied = false;
        if let (false, Some(judged)) = (unchecked, &script.judged) {
            let mut pieces: Vec<String> = match candidate.cds.as_slice() {
                [] => Vec::new(),
                [one] => vec![(*one).to_owned()],
                _ => vec![format!("cd \"${VALUE}\"")],
            };
            pieces.push(judged.clone());
            let inner = pieces.join(joiner);
            match crate::bash::check_at(&inner, inputs, depth + 1) {
                found @ Check::Ask {
                    kind: AskKind::Immune,
                    ..
                } => return Some(found),
                Check::Deny { .. } => denied = true,
                _ => {}
            }
        }
        if (unchecked || denied) && runs_rm(target, patterns) {
            let message = if denied && !unchecked {
                DENIED_INSIDE
            } else {
                UNCHECKED
            };
            return Some(Check::Ask {
                message: message.to_owned(),
                kind: AskKind::Immune,
                suggestions: Vec::new(),
            });
        }
    }
    None
}
