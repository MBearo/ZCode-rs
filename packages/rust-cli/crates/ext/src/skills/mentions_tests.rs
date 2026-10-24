//! 输入中间的 `/<skill 名>`（ext.md §2.8、context.md §4.4）。

use std::sync::Arc;

use rust_cli_kernel_api::testing::MemoryEnvironment;

use super::tokens;
use crate::skills::SkillCatalog;
use crate::test_support::{CWD, facts, workspace};

fn names(input: &str) -> Vec<&str> {
    tokens(input)
        .into_iter()
        .map(|token| &input[token.1..token.2])
        .collect()
}

#[test]
fn a_name_needs_a_space_or_the_edge_on_both_sides() {
    assert_eq!(names("run /greet now"), ["greet"]);
    assert_eq!(names("see /greet"), ["greet"]);
    // 后面紧跟标点、前面是中文标点或别的字符的都不算。
    assert!(names("run /greet. and /greet, and x/greet").is_empty());
    assert!(names("说。/greet 好").is_empty());
    // 名字的字符：字母开头，之后字母、数字、`.`、`:`、`-`、`_`；末尾的 `.` 不算名字。
    assert_eq!(names("try /demo:hello-x_1 ok"), ["demo:hello-x_1"]);
    assert!(names("try /1abc ok").is_empty());
}

#[test]
fn the_command_at_the_start_is_skipped() {
    assert_eq!(names("/greet Ada and /plain"), ["plain"]);
    // 开头只隔空白的几个都算开头的命令。
    assert!(names("/greet /plain").is_empty());
    assert_eq!(names("x /greet /plain"), ["greet", "plain"]);
}

#[tokio::test]
async fn only_skills_are_mentioned_once_each_in_order() {
    let dir = format!("{CWD}/.zcode/skills");
    let env = MemoryEnvironment::new(facts())
        .with_file(
            &format!("{dir}/greet/SKILL.md"),
            "---\ndescription: G\n---\n",
        )
        .with_file(
            &format!("{dir}/helper/SKILL.md"),
            "---\ndescription: H\ndisable-model-invocation: true\n---\n",
        );
    let env = Arc::new(env);
    let catalog = SkillCatalog::new(env.clone(), env);
    assert_eq!(
        catalog
            .mentions(
                &workspace(),
                "use /helper then /greet, /greet and /nope or /greet"
            )
            .await,
        ["helper", "greet"]
    );
    // print 会话（`-p`、stream-json）的输入不算人类键入。
    catalog.set_print_session(true);
    assert!(
        catalog
            .mentions(&workspace(), "use /greet now")
            .await
            .is_empty()
    );
}
