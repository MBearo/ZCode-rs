use super::*;

fn token(text: &str) -> Option<(String, bool)> {
    let cursor = text.find('|').unwrap_or(text.len());
    let clean = text.replace('|', "");
    at_token(&clean, cursor).map(|t| (clean[t.start..t.end].to_owned(), t.quoted))
}

#[test]
fn the_token_needs_a_leading_at_after_whitespace() {
    assert_eq!(token("see @src/ma|"), Some(("@src/ma".to_owned(), false)));
    assert_eq!(token("@"), Some(("@".to_owned(), false)));
    assert_eq!(token("mail a@b|"), None);
    assert_eq!(token("@a b|"), None);
}

#[test]
fn the_token_extends_past_the_cursor() {
    assert_eq!(
        token("x @sr|c/main.rs tail"),
        Some(("@src/main.rs".to_owned(), false))
    );
}

#[test]
fn quoted_tokens_keep_their_quotes() {
    assert_eq!(token("@\"my fi|"), Some(("@\"my fi".to_owned(), true)));
    assert_eq!(
        token("@\"my fi|le.txt\" x"),
        Some(("@\"my file.txt\"".to_owned(), true))
    );
    let parsed = at_token("@\"a b", 5).unwrap();
    assert_eq!(parsed.query, "a b");
}

#[test]
fn replacements_quote_paths_with_spaces() {
    assert_eq!(replacement("src/main.rs", false, true), "@src/main.rs ");
    assert_eq!(
        replacement("my dir/f.txt", false, true),
        "@\"my dir/f.txt\" "
    );
    assert_eq!(replacement("src/", false, false), "@src/");
    assert_eq!(replacement("a", true, false), "@\"a\"");
    assert_eq!(
        common_prefix(&["src/a.rs".into(), "src/ab.rs".into()]),
        "src/a"
    );
    assert_eq!(common_prefix(&[]), "");
}
