use super::*;

#[test]
fn heredoc_delimiters() {
    assert_eq!(heredoc_quoted("mytool <<EOF"), Some(false));
    assert_eq!(heredoc_quoted("mytool <<-EOF"), Some(false));
    assert_eq!(heredoc_quoted("mytool << 'EOF'"), Some(true));
    assert_eq!(heredoc_quoted("mytool <<\"EOF\""), Some(true));
    assert_eq!(heredoc_quoted("mytool <<\\EOF"), Some(true));
    assert_eq!(heredoc_quoted("mytool <<<word"), None);
    assert_eq!(heredoc_quoted("mytool x"), None);
}

#[test]
fn expansions_outside_single_quotes() {
    for text in [
        "a $X", "a ${X}", "a $(b)", "a `b`", "a \"$1\"", "a $@", "a=$X",
    ] {
        assert!(has_expansion(text), "{text}");
    }
    for text in [
        "a '$X'",
        "a \\$X",
        "a $((1+2))",
        "a \"b$\"",
        "a $",
        "a ~/x *.txt",
    ] {
        assert!(!has_expansion(text), "{text}");
    }
}

#[test]
fn subcommand_texts() {
    assert_eq!(subcommand_text("a \\\n  b"), "a b");
    assert_eq!(subcommand_text("a\t\\\n\tb \\\nc"), "a b c");
    assert_eq!(subcommand_text("mytool <<'EOF'"), "mytool");
    assert_eq!(
        subcommand_text("mytool x <<'EOF' --flag"),
        "mytool x --flag"
    );
    assert_eq!(subcommand_text("mytool <<EOF"), "mytool <<EOF");
    assert_eq!(subcommand_text("  a  "), "a");
}
