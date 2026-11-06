//! 代码块的语法着色（docs/puffin-tui.md §4.2「回答的 Markdown」）：八类记号、几种语言、跨行的构造。
//! 移植自 packages/tui/tui/test/src/zcode/tui/markdown/SyntaxSuite.scala（feat/tui a2bf878b34）。
use super::syntax::Kind::{self, *};
use super::syntax::{Tok, lex};

fn lexed<'a>(lang: &str, code: &'a str) -> Vec<Vec<Tok<'a>>> {
    lex(lang, code).unwrap_or_else(|| panic!("{lang} is not known"))
}

/// 每行里不是普通文字的记号。
fn marked<'a>(lang: &str, code: &'a str) -> Vec<Vec<(&'a str, Kind)>> {
    lexed(lang, code)
        .into_iter()
        .map(|line| {
            line.into_iter()
                .filter(|t| t.kind != Plain)
                .map(|t| (t.text, t.kind))
                .collect()
        })
        .collect()
}

#[test]
fn an_unknown_language_has_no_tokens_names_are_case_insensitive_and_have_aliases() {
    assert_eq!(lex("cobol", "MOVE 1"), None);
    for lang in [
        "ts",
        "TypeScript",
        "tsx",
        "js",
        "javascript",
        "jsx",
        "mjs",
        "cjs",
        "scala",
        "sc",
        "python",
        "py",
        "rust",
        "rs",
        "go",
        "sh",
        "bash",
        "zsh",
        "shell",
        "json",
        "jsonc",
    ] {
        assert!(lex(lang, "x").is_some(), "{lang}");
    }
}

#[test]
fn the_tokens_of_a_line_put_back_together_are_the_line() {
    let code = "export const n = 0x1F + 2.5 // ok\n  return \"a\\\"b\" /* c */ + `t`";
    let joined: Vec<String> = lexed("ts", code)
        .iter()
        .map(|l| l.iter().map(|t| t.text).collect())
        .collect();
    assert_eq!(joined, code.split('\n').collect::<Vec<_>>());
}

#[test]
fn keywords_strings_numbers_comments_a_keyword_inside_a_name_is_not_one() {
    assert_eq!(
        marked("ts", "const constant = 'a' + 1_000 // note"),
        vec![vec![
            ("const", Keyword),
            ("'a'", Str),
            ("1_000", Num),
            ("// note", Comment)
        ]]
    );
}

#[test]
fn numbers_keep_their_whole_literal_a_digit_inside_a_name_is_not_one() {
    assert_eq!(
        marked("ts", "0x1F 2.5 x1"),
        vec![vec![("0x1F", Num), ("2.5", Num)]]
    );
}

#[test]
fn a_string_with_no_closing_quote_on_its_line_is_not_a_string() {
    assert_eq!(marked("ts", "a = \"open"), vec![vec![]]);
}

#[test]
fn block_comments_and_template_strings_run_across_lines() {
    assert_eq!(
        marked("ts", "/* a\nb */ x"),
        vec![vec![("/* a", Comment)], vec![("b */", Comment)]]
    );
    assert_eq!(
        marked("js", "s = `a\nb` + 1"),
        vec![vec![("`a", Str)], vec![("b`", Str), ("1", Num)]]
    );
}

#[test]
fn scala_triple_quoted_strings_run_across_lines() {
    assert_eq!(
        marked("scala", "val s = \"\"\"a\nb\"\"\""),
        vec![
            vec![("val", Keyword), ("\"\"\"a", Str)],
            vec![("b\"\"\"", Str)]
        ]
    );
}

#[test]
fn python_hash_comments_and_triple_quotes() {
    assert_eq!(
        marked("py", "def f(): # c"),
        vec![vec![("def", Keyword), ("f", Func), ("# c", Comment)]]
    );
    assert_eq!(
        marked("python", "'''a\nb''' if x"),
        vec![vec![("'''a", Str)], vec![("b'''", Str), ("if", Keyword)]]
    );
}

#[test]
fn rust_a_lifetime_is_not_a_string_a_char_literal_is() {
    assert_eq!(
        marked("rust", "fn f<'a>(x: &'a str) -> char { 'x' }"),
        vec![vec![("fn", Keyword), ("'x'", Str)]]
    );
}

#[test]
fn go_raw_strings_run_across_lines() {
    assert_eq!(
        marked("go", "s := `a\nb`"),
        vec![vec![("`a", Str)], vec![("b`", Str)]]
    );
}

#[test]
fn shell_hash_starts_a_comment_only_at_the_start_of_a_word() {
    assert_eq!(
        marked("sh", "if echo a#b \"$HOME\"; then # note"),
        vec![vec![
            ("if", Keyword),
            ("\"$HOME\"", Str),
            ("then", Keyword),
            ("# note", Comment)
        ]]
    );
}

#[test]
fn json_keys_then_strings_numbers_and_constants_as_values() {
    assert_eq!(
        marked(
            "json",
            "{\"k\": \"v\", \"n\": -1, \"t\": true, \"z\" : null}"
        ),
        vec![vec![
            ("\"k\"", Key),
            ("\"v\"", Str),
            ("\"n\"", Key),
            ("1", Num),
            ("\"t\"", Key),
            ("true", Const),
            ("\"z\"", Key),
            ("null", Const),
        ]]
    );
}

#[test]
fn a_name_right_before_paren_is_a_function_a_keyword_before_paren_is_still_a_keyword() {
    assert_eq!(
        marked("ts", "if (ok) parse(x); f (y)"),
        vec![vec![("if", Keyword), ("parse", Func)]]
    );
    assert_eq!(
        marked("go", "func main() { fmt.Println(1) }"),
        vec![vec![
            ("func", Keyword),
            ("main", Func),
            ("Println", Func),
            ("1", Num)
        ]]
    );
}

#[test]
fn ts_scala_and_rust_a_name_that_starts_in_upper_case_is_a_type() {
    assert_eq!(
        marked("ts", "const m: Map<string, Item> = new Map()"),
        vec![vec![
            ("const", Keyword),
            ("Map", Type),
            ("Item", Type),
            ("new", Keyword),
            ("Map", Func)
        ]]
    );
    assert_eq!(
        marked("scala", "val xs: List[Int] = Nil"),
        vec![vec![
            ("val", Keyword),
            ("List", Type),
            ("Int", Type),
            ("Nil", Type)
        ]]
    );
    assert_eq!(
        marked("rust", "let v: Vec<u8> = Vec::new();"),
        vec![vec![
            ("let", Keyword),
            ("Vec", Type),
            ("Vec", Type),
            ("new", Func)
        ]]
    );
    // 别的语言里大写开头只是名字。
    assert_eq!(marked("py", "x = Path"), vec![vec![]]);
}

#[test]
fn literal_words_are_constants_not_keywords() {
    assert_eq!(
        marked("ts", "x = true ?? null ?? undefined"),
        vec![vec![("true", Const), ("null", Const), ("undefined", Const)]]
    );
    assert_eq!(
        marked("py", "if x is None: return False"),
        vec![vec![
            ("if", Keyword),
            ("is", Keyword),
            ("None", Const),
            ("return", Keyword),
            ("False", Const)
        ]]
    );
    assert_eq!(
        marked("go", "if err != nil { return false }"),
        vec![vec![
            ("if", Keyword),
            ("nil", Const),
            ("return", Keyword),
            ("false", Const)
        ]]
    );
    assert_eq!(
        marked("scala", "if a then true else null"),
        vec![vec![
            ("if", Keyword),
            ("then", Keyword),
            ("true", Const),
            ("else", Keyword),
            ("null", Const)
        ]]
    );
}

/* ---- 移植时加的：Scala 的 UTF-16 语义（以 Scala.js 实跑的结果为准） ---- */

#[test]
fn a_char_literal_of_a_character_outside_the_bmp_is_not_a_string() {
    // Scala 看 charAt(i + 2) 时那里是代理项：'😀' 不是字符字面量；转义的 '\😀' 与 BMP 的 'é' 是。
    assert_eq!(
        lexed("rust", "let c = '😀'; '\\😀' 'é'"),
        vec![vec![
            Tok {
                text: "let",
                kind: Keyword
            },
            Tok {
                text: " c = '😀'; ",
                kind: Plain
            },
            Tok {
                text: "'\\😀'",
                kind: Str
            },
            Tok {
                text: " ",
                kind: Plain
            },
            Tok {
                text: "'é'",
                kind: Str
            },
        ]]
    );
}

#[test]
fn a_carried_construct_on_an_empty_line_is_an_empty_token() {
    assert_eq!(
        lexed("ts", "/*\n\n*/"),
        vec![
            vec![Tok {
                text: "/*",
                kind: Comment
            }],
            vec![Tok {
                text: "",
                kind: Comment
            }],
            vec![Tok {
                text: "*/",
                kind: Comment
            }],
        ]
    );
}
