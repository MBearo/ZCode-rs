use super::{split, tools};

#[test]
fn tools_are_a_comma_list_or_a_list() {
    let (map, _) =
        split("---\nname: a\ntools: Read, Grep\ndisallowedTools:\n  - Bash\n---\n").unwrap();
    assert_eq!(
        tools(map.get("tools")),
        Some(vec!["Read".to_owned(), "Grep".to_owned()])
    );
    assert_eq!(
        tools(map.get("disallowedTools")),
        Some(vec!["Bash".to_owned()])
    );
}

#[test]
fn star_or_missing_tools_mean_all_tools() {
    let (map, _) = split("---\nname: a\ntools: [Read, \"*\"]\n---\n").unwrap();
    assert_eq!(tools(map.get("tools")), None);
    assert_eq!(tools(None), None);
}

/// 嵌套的对象在 agent 文件里按没写（以前 agents 自己那份解析器丢掉这样的键）：工具清单是全部、不是一个怪名字。
#[test]
fn a_nested_block_counts_as_absent() {
    let (map, _) =
        split("---\nname: a\ntools:\n  Read: true\ndescription:\n  nested text\n---\n").unwrap();
    assert_eq!(tools(map.get("tools")), None);
    assert_eq!(map.get("description").and_then(|value| value.text()), None);
}
