//! 最近的名字（编辑距离不超过 2 的名字或别名）：Skill 工具的未知名字与 `-p` 下未知
//! 斜杠命令的提醒都用它（F58：以前 `tool.rs` 与 `unknown.rs` 各写一份编辑距离与候选）。

/// 编辑距离（Levenshtein，按 char）。
pub(super) fn distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut row: Vec<usize> = (0..=right.len()).collect();
    for (i, a) in left.chars().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, b) in right.iter().enumerate() {
            let current = row[j + 1];
            row[j + 1] = if a == *b {
                previous
            } else {
                1 + previous.min(row[j]).min(row[j + 1])
            };
            previous = current;
        }
    }
    row[right.len()]
}

/// 不分大小写、编辑距离不超过 2 的最近的名字；距离相同时取先出现的。
pub(super) fn closest<'a>(name: &str, names: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let wanted = name.to_lowercase();
    names
        .into_iter()
        .map(|candidate| (distance(&wanted, &candidate.to_lowercase()), candidate))
        .filter(|(distance, _)| *distance <= 2)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nearest_name_within_two_edits_wins_and_ties_keep_the_first() {
        assert_eq!(distance("kitten", "sitting"), 3);
        assert_eq!(distance("", "abc"), 3);
        assert_eq!(distance("字符", "字"), 1);
        assert_eq!(closest("lop", ["loop", "greet"]), Some("loop"));
        assert_eq!(closest("LOOP", ["Loop"]), Some("Loop"));
        assert_eq!(closest("nope", ["loop", "greet"]), None);
        assert_eq!(closest("ab", ["ax", "ay"]), Some("ax"));
    }
}
