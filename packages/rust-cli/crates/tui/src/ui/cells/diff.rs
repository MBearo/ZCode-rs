// 复制自 apps/zcode-host/crates/tui/src/ui/cells/diff.rs（feat/rs 79b6691c55）。
//! 配对的删除 / 新增行按词比较（docs/specs/rust-tui.md §8.2）：改动的词再加深一档底色；改动超过这一
//! 行的 40% 时不按词高亮（整行都变了，按词标出来只会更乱）。

/// 改动的字节范围。
type Ranges = Vec<(usize, usize)>;

/// 改动占比的上限。
const THRESHOLD: f64 = 0.4;
/// 一行最多参与比较的词数；更长的行不按词高亮（比较是平方复杂度）。
const MAX_TOKENS: usize = 256;

/// 词：连续的字母、数字与下划线；连续的空白；或单个其他字符。返回字节范围。
fn tokens(text: &str) -> Vec<(usize, usize)> {
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut previous = None;
    for (index, c) in text.char_indices() {
        let kind = class(c);
        match out.last_mut() {
            Some(last) if previous == Some(kind) && kind != 2 => last.1 = index + c.len_utf8(),
            _ => out.push((index, index + c.len_utf8())),
        }
        previous = Some(kind);
    }
    out
}

/// 不在最长公共子序列里的词合并成的字节范围：（旧行，新行）。
fn unmatched(old: &str, new: &str, a: &[(usize, usize)], b: &[(usize, usize)]) -> (Ranges, Ranges) {
    let word = |text: &str, (s, e): (usize, usize)| text[s..e].to_owned();
    let a_words: Vec<String> = a.iter().map(|r| word(old, *r)).collect();
    let b_words: Vec<String> = b.iter().map(|r| word(new, *r)).collect();
    let (n, m) = (a.len(), b.len());
    let mut table = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if a_words[i] == b_words[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let (mut keep_a, mut keep_b) = (vec![false; n], vec![false; m]);
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a_words[i] == b_words[j] {
            keep_a[i] = true;
            keep_b[j] = true;
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    let ranges = |words: &[(usize, usize)], keep: &[bool]| {
        let mut out: Vec<(usize, usize)> = Vec::new();
        for (range, kept) in words.iter().zip(keep) {
            if *kept {
                continue;
            }
            match out.last_mut() {
                Some(last) if last.1 == range.0 => last.1 = range.1,
                _ => out.push(*range),
            }
        }
        out
    };
    (ranges(a, &keep_a), ranges(b, &keep_b))
}

/// 两行里改动的字节范围：（旧行，新行）。改动太多或行太长时 None。
pub fn changed(old: &str, new: &str) -> Option<(Ranges, Ranges)> {
    let (a, b) = (tokens(old), tokens(new));
    if a.len() > MAX_TOKENS || b.len() > MAX_TOKENS {
        return None;
    }
    let (old_ranges, new_ranges) = unmatched(old, new, &a, &b);
    let size = |ranges: &[(usize, usize)], text: &str| {
        ranges
            .iter()
            .map(|(s, e)| text[*s..*e].chars().count())
            .sum::<usize>()
    };
    let total = old.chars().count() + new.chars().count();
    let changed = size(&old_ranges, old) + size(&new_ranges, new);
    if total == 0 || changed as f64 / total as f64 > THRESHOLD {
        return None;
    }
    Some((old_ranges, new_ranges))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_changed_words_are_marked() {
        let (old, new) = changed("let a = 1;", "let a = 2;").unwrap();
        assert_eq!(old, [(8, 9)]);
        assert_eq!(new, [(8, 9)]);
        // 整行都变了：不按词高亮。
        assert_eq!(changed("abc", "xyz"), None);
        // 只新增词。
        let (old, new) = changed("call(a)", "call(a, b)").unwrap();
        assert!(old.is_empty());
        assert_eq!(&"call(a, b)"[new[0].0..new[0].1], ", b");
    }
}
