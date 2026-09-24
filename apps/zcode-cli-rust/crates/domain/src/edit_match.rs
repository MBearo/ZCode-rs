//! Edit `old_string` matching (Node `core/src/tool/edit-matchers.ts`) with JS
//! string semantics: JS `trim`, UTF-16 lengths and `\p{L}` letters.
use std::sync::OnceLock;
pub use text::js_trim;
use text::{strip_line_numbers, unescape_unicode, unescape_visible};

#[path = "edit_text.rs"]
mod text;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    Exact,
    QuoteNormalized,
    LineNumberPrefixStripped,
    EscapeNormalized,
    UnicodeEscapeNormalized,
    LineTrimmed,
    IndentationFlexible,
    BlockAnchor,
}

impl Strategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::QuoteNormalized => "quote_normalized",
            Self::LineNumberPrefixStripped => "line_number_prefix_stripped",
            Self::EscapeNormalized => "escape_normalized",
            Self::UnicodeEscapeNormalized => "unicode_escape_normalized",
            Self::LineTrimmed => "line_trimmed",
            Self::IndentationFlexible => "indentation_flexible",
            Self::BlockAnchor => "block_anchor",
        }
    }

    fn broad(self) -> bool {
        matches!(
            self,
            Self::LineTrimmed | Self::IndentationFlexible | Self::BlockAnchor
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Match {
    Matched {
        actual: String,
        strategy: Strategy,
        candidates: usize,
    },
    Ambiguous {
        strategy: Strategy,
        candidates: usize,
    },
    NotFound,
}

const FALLBACKS: [Strategy; 7] = [
    Strategy::QuoteNormalized,
    Strategy::LineNumberPrefixStripped,
    Strategy::EscapeNormalized,
    Strategy::UnicodeEscapeNormalized,
    Strategy::LineTrimmed,
    Strategy::IndentationFlexible,
    Strategy::BlockAnchor,
];
const BLOCK_ANCHOR_MIN_SIMILARITY: f64 = 0.8;
const LEFT_SINGLE: char = '‘';
const RIGHT_SINGLE: char = '’';
const LEFT_DOUBLE: char = '“';
const RIGHT_DOUBLE: char = '”';

/// Node `findEditMatch`.
pub fn find(content: &str, search: &str, replace_all: bool) -> Match {
    let exact = substrings(content, search);
    if !exact.is_empty() {
        return result(Strategy::Exact, exact);
    }
    for strategy in FALLBACKS {
        if replace_all && strategy.broad() {
            continue;
        }
        let candidates = collect(strategy, content, search);
        if !candidates.is_empty() {
            return result(strategy, candidates);
        }
    }
    Match::NotFound
}

/// Node `normalizeReplacementForMatch`.
pub fn normalize_replacement(strategy: Strategy, new_string: &str) -> String {
    if strategy == Strategy::EscapeNormalized {
        unescape_visible(new_string)
    } else {
        new_string.to_owned()
    }
}

/// Node `preserveQuoteStyle`.
pub fn preserve_quote_style(old: &str, actual: &str, new_string: &str) -> String {
    if old == actual {
        return new_string.to_owned();
    }
    let mut result = new_string.to_owned();
    if actual.contains([LEFT_DOUBLE, RIGHT_DOUBLE]) {
        result = curly(&result, '"', LEFT_DOUBLE, RIGHT_DOUBLE, false);
    }
    if actual.contains([LEFT_SINGLE, RIGHT_SINGLE]) {
        result = curly(&result, '\'', LEFT_SINGLE, RIGHT_SINGLE, true);
    }
    result
}

fn result(strategy: Strategy, candidates: Vec<String>) -> Match {
    let first = &candidates[0];
    if candidates.iter().any(|c| c != first) {
        return Match::Ambiguous {
            strategy,
            candidates: candidates.len(),
        };
    }
    Match::Matched {
        actual: first.clone(),
        strategy,
        candidates: candidates.len(),
    }
}

fn collect(strategy: Strategy, content: &str, search: &str) -> Vec<String> {
    match strategy {
        Strategy::Exact => substrings(content, search),
        Strategy::QuoteNormalized => quote_normalized(content, search),
        Strategy::LineNumberPrefixStripped => match strip_line_numbers(search) {
            Some(stripped) if stripped != search => substrings(content, &stripped),
            _ => vec![],
        },
        Strategy::EscapeNormalized => {
            let unescaped = unescape_visible(search);
            if unescaped == search {
                return vec![];
            }
            substrings(content, &unescaped)
        }
        Strategy::UnicodeEscapeNormalized => match unescape_unicode(search) {
            Some(unescaped) if unescaped != search => substrings(content, &unescaped),
            _ => vec![],
        },
        Strategy::LineTrimmed | Strategy::IndentationFlexible | Strategy::BlockAnchor => {
            blocks(strategy, content, search)
        }
    }
}

/// Non-overlapping occurrences (`collectSubstringCandidates`).
fn substrings(content: &str, search: &str) -> Vec<String> {
    if search.is_empty() {
        return vec![];
    }
    content
        .match_indices(search)
        .map(|(_, s)| s.to_owned())
        .collect()
}

fn normalize_quote(c: char) -> char {
    match c {
        LEFT_SINGLE | RIGHT_SINGLE => '\'',
        LEFT_DOUBLE | RIGHT_DOUBLE => '"',
        c => c,
    }
}

/// Matches in quote-normalized text; each candidate is the original slice of
/// the same length (normalization maps one character to one character).
/// Matches do not overlap, so one forward cursor maps character positions
/// back to byte offsets.
fn quote_normalized(content: &str, search: &str) -> Vec<String> {
    let normalized: String = content.chars().map(normalize_quote).collect();
    let needle: String = search.chars().map(normalize_quote).collect();
    if needle.is_empty() {
        return vec![];
    }
    let width = search.chars().count();
    let mut offsets = content
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(content.len()))
        .peekable();
    let mut position = 0usize;
    let mut byte_at = |target: usize| {
        while position < target {
            offsets.next();
            position += 1;
        }
        offsets.peek().copied().unwrap_or(content.len())
    };
    let (mut chars_before, mut scanned) = (0, 0);
    normalized
        .match_indices(&needle)
        .map(|(at, _)| {
            chars_before += normalized[scanned..at].chars().count();
            scanned = at;
            let start = byte_at(chars_before);
            let end = byte_at(chars_before + width);
            content[start..end].to_owned()
        })
        .collect()
}

fn blocks(strategy: Strategy, content: &str, search: &str) -> Vec<String> {
    let lines: Vec<&str> = content.split('\n').collect();
    let mut wanted: Vec<&str> = search.split('\n').collect();
    if wanted.last() == Some(&"") {
        wanted.pop();
    }
    let minimum = match strategy {
        Strategy::LineTrimmed => 1,
        Strategy::IndentationFlexible => 2,
        _ => 3,
    };
    if wanted.len() < minimum || wanted.len() > lines.len() {
        return vec![];
    }
    let normalized = (strategy == Strategy::IndentationFlexible).then(|| without_indent(&wanted));
    let (first, last) = (js_trim(wanted[0]), js_trim(wanted[wanted.len() - 1]));
    (0..=lines.len() - wanted.len())
        .filter_map(|start| {
            let block = &lines[start..start + wanted.len()];
            let hit = match strategy {
                Strategy::LineTrimmed => block
                    .iter()
                    .zip(&wanted)
                    .all(|(a, b)| js_trim(a) == js_trim(b)),
                Strategy::IndentationFlexible => Some(without_indent(block)) == normalized,
                _ => {
                    js_trim(block[0]) == first
                        && js_trim(block[block.len() - 1]) == last
                        && middle_similarity(block, &wanted) >= BLOCK_ANCHOR_MIN_SIMILARITY
                }
            };
            hit.then(|| block.join("\n"))
        })
        .collect()
}

/// `removeCommonIndent`: only tabs and spaces count as indentation.
fn without_indent(lines: &[&str]) -> String {
    let indent = |line: &str| {
        line.bytes()
            .take_while(|b| *b == b'\t' || *b == b' ')
            .count()
    };
    let min = lines
        .iter()
        .filter(|l| !js_trim(l).is_empty())
        .map(|l| indent(l))
        .min();
    let Some(min) = min else {
        return lines.join("\n");
    };
    lines
        .iter()
        .map(|l| if js_trim(l).is_empty() { l } else { &l[min..] })
        .collect::<Vec<_>>()
        .join("\n")
}

fn middle_similarity(actual: &[&str], expected: &[&str]) -> f64 {
    if actual.len() <= 2 {
        return 1.0;
    }
    let middle = 1..actual.len() - 1;
    let count = middle.len();
    let total: f64 = middle
        .map(|i| similarity(js_trim(actual[i]), js_trim(expected[i])))
        .sum();
    total / count as f64
}

fn similarity(left: &str, right: &str) -> f64 {
    if left == right {
        return 1.0;
    }
    let (a, b): (Vec<u16>, Vec<u16>) = (
        left.encode_utf16().collect(),
        right.encode_utf16().collect(),
    );
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 1.0;
    }
    1.0 - levenshtein(&a, &b) as f64 / longest as f64
}

fn levenshtein(a: &[u16], b: &[u16]) -> usize {
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, y) in b.iter().enumerate() {
            let cost = usize::from(x != y);
            current.push(
                (previous[j + 1] + 1)
                    .min(current[j] + 1)
                    .min(previous[j] + cost),
            );
        }
        previous = current;
    }
    previous[b.len()]
}

/// `/\p{L}/u` on one character.
fn letter(c: Option<char>) -> bool {
    static LETTER: OnceLock<regress::Regex> = OnceLock::new();
    let re = LETTER.get_or_init(|| regress::Regex::with_flags(r"^\p{L}$", "u").unwrap());
    c.is_some_and(|c| re.find(&c.to_string()).is_some())
}

fn opening(chars: &[char], index: usize) -> bool {
    index == 0
        || matches!(
            chars[index - 1],
            ' ' | '\t' | '\n' | '\r' | '(' | '[' | '{' | '—' | '–'
        )
}

/// `applyCurlyDoubleQuotes` / `applyCurlySingleQuotes`.
fn curly(value: &str, straight: char, left: char, right: char, apostrophe: bool) -> String {
    let chars: Vec<char> = value.chars().collect();
    (0..chars.len())
        .map(|i| {
            if chars[i] != straight {
                return chars[i];
            }
            let previous = i.checked_sub(1).map(|p| chars[p]);
            if apostrophe && letter(previous) && letter(chars.get(i + 1).copied()) {
                return right;
            }
            if opening(&chars, i) { left } else { right }
        })
        .collect()
}

#[cfg(test)]
#[path = "edit_match_tests.rs"]
mod tests;
