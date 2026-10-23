//! 命令行的切分规则：照 commander 12（`parseOptions`、`optionMissingArgument`、`unknownOption`、`suggestSimilar`），
//! docs/specs/rust-cli/cli-args.md §2。
//!
//! 只管切分：哪个参数是哪个选项的值、哪些是位置参数、哪些是不认识的选项；选项的含义由调用方
//! （`args.rs`）在每个选项出现时处理（commander 的 `option:<name>` 事件），出错就立即停下，与 commander
//! 在切分过程中抛错的时机相同。

/// 选项带不带值（commander 的 `<value>`、`[value]`、`<value...>` 与布尔选项）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Arity {
    Flag,
    /// `<value>`：取下一个参数，不管它是不是以 `-` 开头。
    Required,
    /// `[value]`：下一个参数不像选项时才取。
    Optional,
    /// `<value...>`：先取下一个参数，之后不像选项的参数都归它，直到下一个选项。
    Variadic,
}

/// 一个选项的写法。
#[derive(Debug)]
pub(crate) struct OptionSpec<K: 'static> {
    pub(crate) key: K,
    /// commander 的 flags 原文（报错里原样出现），例如 `--max-turns <turns>`。
    pub(crate) flags: &'static str,
    /// 短写法，例如 `-p`。
    pub(crate) short: Option<&'static str>,
    /// 长写法（第一个是主名，其余是别名，例如 `--allowedTools` 与 `--allowed-tools`）。
    pub(crate) longs: &'static [&'static str],
    pub(crate) arity: Arity,
}

impl<K> OptionSpec<K> {
    fn is(&self, arg: &str) -> bool {
        self.short == Some(arg) || self.longs.contains(&arg)
    }

    fn takes_value(&self) -> bool {
        !matches!(self.arity, Arity::Flag)
    }
}

/// 切分的结果：位置参数与不认识的参数（第一个不认识的选项及其后的全部参数，commander 的 `unknown`）。
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Parsed {
    pub(crate) operands: Vec<String>,
    pub(crate) unknown: Vec<String>,
}

/// commander 的 `maybeOption`：长度大于 1、以 `-` 开头。
fn maybe_option(arg: &str) -> bool {
    arg.len() > 1 && arg.starts_with('-')
}

/// 按 commander 12 的 `parseOptions` 切分。每个认出的选项（与它的值）交给 `on_option`，它返回的错误原样
/// 向上传；缺值时报 `error: option '<flags>' argument missing`。
pub(crate) fn split<K, E>(
    args: &[String],
    specs: &[OptionSpec<K>],
    mut on_option: impl FnMut(&OptionSpec<K>, Option<&str>) -> Result<(), E>,
    missing: impl Fn(String) -> E,
) -> Result<Parsed, E> {
    let find = |arg: &str| specs.iter().find(|spec| spec.is(arg));
    let mut parsed = Parsed::default();
    let mut into_unknown = false;
    let mut rest: std::collections::VecDeque<String> = args.iter().cloned().collect();
    let mut variadic: Option<&OptionSpec<K>> = None;
    while let Some(arg) = rest.pop_front() {
        // `--` 之后都是位置参数（落在 unknown 里时连 `--` 一起留下）。
        if arg == "--" {
            let dest = if into_unknown {
                parsed.unknown.push(arg);
                &mut parsed.unknown
            } else {
                &mut parsed.operands
            };
            dest.extend(rest.drain(..));
            break;
        }
        if let Some(spec) = variadic
            && !maybe_option(&arg)
        {
            on_option(spec, Some(&arg))?;
            continue;
        }
        variadic = None;
        if maybe_option(&arg)
            && let Some(spec) = find(&arg)
        {
            match spec.arity {
                Arity::Flag => on_option(spec, None)?,
                Arity::Required | Arity::Variadic => match rest.pop_front() {
                    Some(value) => on_option(spec, Some(&value))?,
                    None => {
                        return Err(missing(format!(
                            "error: option '{}' argument missing",
                            spec.flags
                        )));
                    }
                },
                Arity::Optional => {
                    let value = match rest.front() {
                        Some(next) if !maybe_option(next) => rest.pop_front(),
                        _ => None,
                    };
                    on_option(spec, value.as_deref())?;
                }
            }
            if spec.arity == Arity::Variadic {
                variadic = Some(spec);
            }
            continue;
        }
        // 单个 `-` 后跟多个字符：先认第一个短选项（`-pc`、`-r<值>`）。
        if arg.len() > 2
            && arg.starts_with('-')
            && !arg.starts_with("--")
            && let Some(first) = arg.get(..2)
            && let Some(spec) = find(first)
        {
            let tail = &arg[2..];
            if spec.takes_value() {
                // commander 的 `_combineFlagAndOptionalValue` 缺省为真：可选值也接在同一个参数里。
                on_option(spec, Some(tail))?;
            } else {
                on_option(spec, None)?;
                rest.push_front(format!("-{tail}"));
            }
            continue;
        }
        // `--name=value`。
        if arg.starts_with("--")
            && let Some(index) = arg.find('=')
            && index > 2
            && let Some(spec) = find(&arg[..index])
            && spec.takes_value()
        {
            on_option(spec, Some(&arg[index + 1..]))?;
            continue;
        }
        if maybe_option(&arg) {
            into_unknown = true;
        }
        if into_unknown {
            parsed.unknown.push(arg);
        } else {
            parsed.operands.push(arg);
        }
    }
    Ok(parsed)
}

/// commander 的 `suggestSimilar`（编辑距离不超过 3、相似度大于 0.4，取最近的一组，按 `localeCompare` 排序）。
/// 只对 `--` 开头的选项给建议；`candidates` 是帮助里列出的长写法。
pub(crate) fn suggestion(word: &str, candidates: &[&str]) -> String {
    let Some(word) = word.strip_prefix("--") else {
        return String::new();
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut best = 3usize;
    let mut similar: Vec<&str> = Vec::new();
    for candidate in candidates {
        let Some(candidate) = candidate.strip_prefix("--") else {
            continue;
        };
        if !seen.insert(candidate) || candidate.chars().count() <= 1 {
            continue;
        }
        let distance = edit_distance(word, candidate);
        let length = word.chars().count().max(candidate.chars().count());
        #[expect(
            clippy::cast_precision_loss,
            reason = "参数名很短，与 commander 的浮点比较相同"
        )]
        let similarity = (length as f64 - distance as f64) / length as f64;
        if similarity > 0.4 {
            if distance < best {
                best = distance;
                similar = vec![candidate];
            } else if distance == best {
                similar.push(candidate);
            }
        }
    }
    similar.sort_by(|a, b| locale_compare(a, b));
    let named: Vec<String> = similar.iter().map(|name| format!("--{name}")).collect();
    match named.as_slice() {
        [] => String::new(),
        [one] => format!("\n(Did you mean {one}?)"),
        many => format!("\n(Did you mean one of {}?)", many.join(", ")),
    }
}

/// 选项名的排序（JS `localeCompare` 对 ASCII 选项名的结果：先按忽略大小写的字母序，标点排在字母与数字之前；
/// 只差大小写时小写在前）。
pub(crate) fn locale_compare(a: &str, b: &str) -> std::cmp::Ordering {
    let key = |text: &str| -> Vec<(u8, char)> {
        text.chars()
            .map(|c| {
                let class = if c.is_ascii_alphanumeric() { 1 } else { 0 };
                (class, c.to_ascii_lowercase())
            })
            .collect()
    };
    key(a).cmp(&key(b)).then_with(|| b.cmp(a))
}

/// commander 的 `editDistance`（Damerau–Levenshtein 的最优串对齐距离；长度差超过 3 时直接给最大值）。
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > 3 {
        return a.len().max(b.len());
    }
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for j in 1..=b.len() {
        for i in 1..=a.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut value = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                value = value.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = value;
        }
    }
    d[a.len()][b.len()]
}

#[cfg(test)]
#[path = "commander_tests.rs"]
mod tests;
