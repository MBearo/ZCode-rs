//! 行内解析的测试。
//! 移植自 packages/tui/tui/test/src/zcode/tui/markdown/InlineParserSuite.scala（feat/tui a2bf878b34）。
use super::ast::Inline::{self, *};
use super::inline_parser::{parse, plain};

fn t(s: &str) -> Inline {
    Text(s.to_owned())
}

fn link(content: Vec<Inline>, dest: &str, title: Option<&str>) -> Inline {
    Link {
        content,
        dest: dest.to_owned(),
        title: title.map(str::to_owned),
    }
}

fn auto(uri: &str, is_email: bool) -> Inline {
    Autolink {
        uri: uri.to_owned(),
        is_email,
    }
}

#[test]
fn plain_text_is_a_single_text_node() {
    assert_eq!(parse("hello world"), vec![t("hello world")]);
    assert_eq!(parse(""), vec![]);
}

#[test]
fn emphasis_and_strong() {
    assert_eq!(parse("*a*"), vec![Emph(vec![t("a")])]);
    assert_eq!(parse("_a_"), vec![Emph(vec![t("a")])]);
    assert_eq!(parse("**a**"), vec![Strong(vec![t("a")])]);
    assert_eq!(parse("__a__"), vec![Strong(vec![t("a")])]);
}

#[test]
fn triple_emphasis_nests_strong_inside_emph() {
    assert_eq!(parse("***a***"), vec![Emph(vec![Strong(vec![t("a")])])]);
}

#[test]
fn nested_emphasis_inside_emphasis() {
    assert_eq!(
        parse("*a **b** c*"),
        vec![Emph(vec![t("a "), Strong(vec![t("b")]), t(" c")])]
    );
}

#[test]
fn intraword_underscore_does_not_emphasise_asterisk_does() {
    assert_eq!(parse("a_b_c"), vec![t("a_b_c")]);
    assert_eq!(parse("a*b*c"), vec![t("a"), Emph(vec![t("b")]), t("c")]);
}

#[test]
fn unmatched_delimiters_stay_literal() {
    assert_eq!(parse("**bold"), vec![t("**bold")]);
    assert_eq!(parse("a * b"), vec![t("a * b")]);
}

#[test]
fn inline_code_span_verbatim_binds_tighter_than_emphasis() {
    assert_eq!(parse("`a*b*c`"), vec![Code("a*b*c".into())]);
    assert_eq!(
        parse("a `code` b"),
        vec![t("a "), Code("code".into()), t(" b")]
    );
}

#[test]
fn code_span_with_double_backticks_holds_a_single_backtick() {
    assert_eq!(parse("``a`b``"), vec![Code("a`b".into())]);
}

#[test]
fn code_span_strips_one_surrounding_space() {
    assert_eq!(parse("` a `"), vec![Code("a".into())]);
}

#[test]
fn unterminated_code_span_is_literal() {
    assert_eq!(parse("`abc"), vec![t("`abc")]);
}

#[test]
fn strikethrough() {
    assert_eq!(parse("~~gone~~"), vec![Strikethrough(vec![t("gone")])]);
}

#[test]
fn strikethrough_wraps_nested_emphasis() {
    assert_eq!(
        parse("~~a **b**~~"),
        vec![Strikethrough(vec![t("a "), Strong(vec![t("b")])])]
    );
}

#[test]
fn inline_link_with_and_without_title() {
    assert_eq!(
        parse("[x](http://e.com)"),
        vec![link(vec![t("x")], "http://e.com", None)]
    );
    assert_eq!(
        parse("[x](http://e.com \"t\")"),
        vec![link(vec![t("x")], "http://e.com", Some("t"))]
    );
}

#[test]
fn link_text_may_itself_contain_emphasis() {
    assert_eq!(
        parse("[*x*](u)"),
        vec![link(vec![Emph(vec![t("x")])], "u", None)]
    );
}

#[test]
fn image() {
    assert_eq!(
        parse("![alt](img.png)"),
        vec![Image {
            alt: "alt".into(),
            dest: "img.png".into(),
            title: None
        }]
    );
}

#[test]
fn angle_bracket_and_email_autolinks() {
    assert_eq!(parse("<http://e.com>"), vec![auto("http://e.com", false)]);
    assert_eq!(parse("<a@b.com>"), vec![auto("a@b.com", true)]);
}

#[test]
fn a_bare_http_address_is_an_autolink_punctuation_after_it_stays_text() {
    assert_eq!(
        parse("see https://example.com/a."),
        vec![t("see "), auto("https://example.com/a", false), t(".")]
    );
    assert_eq!(
        parse("http://e.com, then"),
        vec![auto("http://e.com", false), t(", then")]
    );
    // 括号成对时留在地址里，多出来的右括号不算。
    assert_eq!(
        parse("(https://e.com/a_(b))"),
        vec![t("("), auto("https://e.com/a_(b)", false), t(")")]
    );
}

#[test]
fn a_bare_address_needs_a_word_start_and_something_after_the_scheme() {
    assert_eq!(parse("xhttps://e.com"), vec![t("xhttps://e.com")]);
    assert_eq!(parse("https:// x"), vec![t("https:// x")]);
    assert_eq!(
        parse("[https://a.com](https://b.com)"),
        vec![link(vec![t("https://a.com")], "https://b.com", None)]
    );
    // 没有闭合的 [ 不是链接文字：后面的网址照样是链接。
    assert_eq!(
        parse("[draft https://e.com"),
        vec![t("[draft "), auto("https://e.com", false)]
    );
}

#[test]
fn unmatched_brackets_are_literal_reference_links_unsupported() {
    assert_eq!(parse("[x]"), vec![t("[x]")]);
    assert_eq!(parse("[x][y]"), vec![t("[x][y]")]);
}

#[test]
fn backslash_escapes_punctuation() {
    assert_eq!(parse("\\*not emph\\*"), vec![t("*not emph*")]);
    assert_eq!(parse("a\\\\b"), vec![t("a\\b")]);
}

#[test]
fn backslash_before_a_non_punctuation_char_stays_literal() {
    assert_eq!(parse("a\\b"), vec![t("a\\b")]);
}

#[test]
fn two_trailing_spaces_make_a_hard_break_a_bare_newline_is_soft() {
    assert_eq!(parse("a  \nb"), vec![t("a"), HardBreak, t("b")]);
    assert_eq!(parse("a\nb"), vec![t("a"), SoftBreak, t("b")]);
}

#[test]
fn backslash_newline_is_a_hard_break() {
    assert_eq!(parse("a\\\nb"), vec![t("a"), HardBreak, t("b")]);
}

/* ---- 移植时加的：链表、字符类与 Scala 的 UTF-16 语义 ---- */

#[test]
fn image_alt_flattens_nested_markup_to_plain_text() {
    assert_eq!(
        parse("![a *b* `c`](i.png)"),
        vec![Image {
            alt: "a *b* c".into(),
            dest: "i.png".into(),
            title: None
        }]
    );
    assert_eq!(plain(&parse("a **b** [c](u)  \nd")), "a b c d");
}

#[test]
fn no_links_inside_links() {
    assert_eq!(
        parse("[a [b](u) c](v)"),
        vec![t("[a "), link(vec![t("b")], "u", None), t(" c](v)")]
    );
}

#[test]
fn emphasis_inside_link_text_resolves_before_the_link_closes() {
    assert_eq!(
        parse("*a [b*](u)"),
        vec![t("*a "), link(vec![t("b*")], "u", None)]
    );
    assert_eq!(
        parse("**a [*b*](u) c**"),
        vec![Strong(vec![
            t("a "),
            link(vec![Emph(vec![t("b")])], "u", None),
            t(" c")
        ])]
    );
}

#[test]
fn link_destinations_and_titles() {
    assert_eq!(parse("[x](<a b>)"), vec![link(vec![t("x")], "a b", None)]);
    assert_eq!(
        parse("[x](a(b)c 'q\\'d')"),
        vec![link(vec![t("x")], "a(b)c", Some("q'd"))]
    );
    assert_eq!(
        parse("[x](a\\)b (t))"),
        vec![link(vec![t("x")], "a)b", Some("t"))]
    );
    assert_eq!(parse("[x](a b)"), vec![t("[x](a b)")]);
}

#[test]
fn rule_of_three_keeps_mixed_runs_apart() {
    assert_eq!(parse("*a**"), vec![Emph(vec![t("a")]), t("*")]);
    assert_eq!(parse("**a*"), vec![t("*"), Emph(vec![t("a")])]);
    assert_eq!(parse("*a**b*"), vec![Emph(vec![t("a**b")])]);
}

#[test]
fn continuation_lines_lose_their_leading_spaces() {
    assert_eq!(parse("a\n   b"), vec![t("a"), SoftBreak, t("b")]);
}

#[test]
fn a_character_outside_the_bmp_is_punctuation_like_a_surrogate() {
    // Scala 按 UTF-16 单元看，emoji 的代理项不是字母也不是空白，算标点：
    // "😀_a_" 的 _ 前面是标点，于是能开强调（若按字母算则不能）。
    assert_eq!(parse("😀_a_"), vec![t("😀"), Emph(vec![t("a")])]);
    assert_eq!(parse("中_a_"), vec![t("中_a_")]);
}

#[test]
fn circled_digits_are_punctuation_for_flanking() {
    // ① 是 No：Java 的 isLetterOrDigit 为假（Rust 的 is_alphanumeric 为真）。
    assert_eq!(parse("①_a_"), vec![t("①"), Emph(vec![t("a")])]);
}

#[test]
fn autolink_scheme_needs_a_letter_first() {
    assert_eq!(parse("<1a:b>"), vec![t("<1a:b>")]);
    assert_eq!(parse("<mailto:a@b.c>"), vec![auto("mailto:a@b.c", false)]);
    assert_eq!(parse("<a@b>"), vec![t("<a@b>")]);
}
