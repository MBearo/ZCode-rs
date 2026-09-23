use super::json_size::serialized_size;
use serde_json::Value;

pub fn page(
    rows: &[Value],
    before: u64,
    limit: usize,
    budget: usize,
) -> serde_json::Result<(Vec<&Value>, bool)> {
    let mut selected = Vec::with_capacity(limit.min(rows.len()));
    let mut bytes = 2;
    let mut more = false;
    // 旧路径为每次剔除一行都编码剩余整页；反向逐行计数保持同一个尾页且只访问一次内容。
    for row in rows
        .iter()
        .rev()
        .filter(|r| r["rowId"].as_u64().unwrap() < before)
    {
        if selected.len() == limit {
            more = true;
            break;
        }
        bytes += serialized_size(row)? + usize::from(!selected.is_empty());
        if bytes > budget {
            more = true;
            break;
        }
        selected.push(row);
    }
    selected.reverse();
    Ok((selected, more))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn page_matches_tail_limit_and_exact_utf8_json_budget() {
        let rows = (1..=12)
            .map(|id| json!({"rowId":id,"text":"汉字\n\"".repeat(id as usize)}))
            .collect::<Vec<_>>();
        for before in [1, 5, 20] {
            for limit in [1, 5, 20] {
                for budget in [2, 100, 1000, 10000] {
                    let eligible = rows
                        .iter()
                        .filter(|r| r["rowId"].as_u64().unwrap() < before)
                        .collect::<Vec<_>>();
                    let mut start = eligible.len().saturating_sub(limit);
                    while start < eligible.len()
                        && serde_json::to_vec(&eligible[start..]).unwrap().len() > budget
                    {
                        start += 1;
                    }
                    let (actual, more) = page(&rows, before, limit, budget).unwrap();
                    assert_eq!(actual, eligible[start..]);
                    assert_eq!(more, start > 0);
                }
            }
        }
    }
}
