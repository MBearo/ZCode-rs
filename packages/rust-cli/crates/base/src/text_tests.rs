use super::*;

/// 收拢前 kernel-api 会话标题的截断（逐字照抄，上限 60）。
fn old_title(compact: &str) -> String {
    if utf16_len(compact) <= 60 {
        compact.to_owned()
    } else {
        format!("{}...", utf16_prefix(compact, 57))
    }
}

/// 收拢前 ext skill 描述的截断（逐字照抄，上限 100）。
fn old_skill(text: &str) -> String {
    if text.chars().count() > 100 {
        format!("{}...", text.chars().take(97).collect::<String>())
    } else {
        text.to_owned()
    }
}

#[test]
fn truncation_matches_the_old_copies() {
    let samples = ["", "a", "中", "😀", "a😀", "é", " "];
    for unit in samples {
        for count in [
            0, 1, 56, 57, 58, 59, 60, 61, 62, 96, 97, 98, 99, 100, 101, 102, 150,
        ] {
            let text = unit.repeat(count);
            assert_eq!(
                truncate_utf16(&text, 60),
                old_title(&text),
                "{unit} × {count}"
            );
            assert_eq!(
                truncate_chars(&text, 100),
                old_skill(&text),
                "{unit} × {count}"
            );
        }
    }
    assert!(matches!(truncate_utf16("short", 60), Cow::Borrowed(_)));
}
