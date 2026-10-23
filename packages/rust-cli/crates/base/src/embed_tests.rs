//! 编入文本的换行规则（修复线 F56，docs/specs/rust-cli/prompts.md「换行」）。
use super::*;

#[test]
fn text_with_only_lf_has_no_cr() {
    assert!(!has_cr(""));
    assert!(!has_cr("select 1;"));
    assert!(!has_cr("create table a (\n  id text\n);\n"));
    // 多字节字符不会被误认成 `\r`。
    assert!(!has_cr("中文\n— ✓ 🙂\n"));
}

#[test]
fn any_cr_is_found_wherever_it_is() {
    assert!(has_cr("\r"));
    assert!(has_cr("a\r\nb"));
    assert!(has_cr("\rstart"));
    assert!(has_cr("end\r"));
    // 单独的 CR（旧 Mac 换行）也算：Node 的模板字符串同样把它规范成 LF。
    assert!(has_cr("a\rb"));
    assert!(has_cr("中文\r\n"));
}

// 编译期可用：断言写在 `const` 里，文本含 `\r` 时编译失败。
const _: () = assert!(!has_cr("a\nb"));
const _: () = assert!(has_cr("a\r\nb"));

#[test]
fn include_lf_gives_the_same_text_as_include_str() {
    // 路径相对调用处的文件，与 `include_str!` 相同。
    let text: &'static str = crate::include_lf!("embed.rs");
    assert_eq!(text, include_str!("embed.rs"));
    assert!(!text.is_empty());
}

// 在 `const` 的初始化里也能用（`TOOL_FILES`、`MIGRATIONS` 是这样用的）。
const IN_CONST: &[&str] = &[crate::include_lf!("embed.rs"), crate::include_lf!("lib.rs")];

#[test]
fn include_lf_works_inside_a_const_initializer() {
    assert_eq!(IN_CONST[0], include_str!("embed.rs"));
    assert_eq!(IN_CONST[1], include_str!("lib.rs"));
}
