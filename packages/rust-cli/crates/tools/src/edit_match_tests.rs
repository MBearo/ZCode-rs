use super::*;

#[test]
fn curly_quotes_match_straight_quotes_and_are_preserved() {
    let content = "say \u{201c}hello\u{201d}\n";
    let actual = find_actual(content, "say \"hello\"").unwrap();
    assert_eq!(actual, "say \u{201c}hello\u{201d}");
    assert_eq!(
        adapt_new("say \"hello\"", &actual, "say \"bye\""),
        "say \u{201c}bye\u{201d}"
    );
    let apostrophe = find_actual("it\u{2019}s 'x'", "it's").unwrap();
    assert_eq!(
        adapt_new("it's", &apostrophe, "that's 'y'"),
        "that\u{2019}s \u{2018}y\u{2019}"
    );
}

#[test]
fn unicode_escapes_match_either_way() {
    assert_eq!(
        find_actual("name = \"é\"", "name = \"\\u00e9\"").unwrap(),
        "name = \"é\""
    );
    assert_eq!(
        find_actual("x = \"\\u00E9\"", "x = \"é\"").unwrap(),
        "x = \"\\u00E9\""
    );
    assert_eq!(
        adapt_new("x = \"é\"", "x = \"\\u00E9\"", "y = \"é\""),
        "y = \"\\u00E9\""
    );
}

#[test]
fn deleting_a_line_takes_its_newline_and_no_ops_fail() {
    assert_eq!(apply("a\nb\nc\n", "b", "", false).unwrap(), "a\nc\n");
    assert_eq!(apply("x x", "x", "y", false).unwrap(), "y x");
    assert_eq!(apply("x x", "x", "y", true).unwrap(), "y y");
    assert_eq!(apply("", "", "new", false).unwrap(), "new");
    assert!(apply("abc", "zzz", "y", false).is_err());
    assert!(applies("a a", "a", true));
    assert!(!applies("a a", "a", false));
}
