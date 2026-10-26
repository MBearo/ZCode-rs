//! 用户调用 skill（ext.md §2.7）：展开的消息形状。

use std::sync::Arc;

use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{MemoryEnvironment, SequentialIds};
use rust_cli_kernel_api::user_command::{
    ExpandedKind, ExpandedMessage, Expansion, LOCAL_COMMAND_CAVEAT_SOURCE, LOCAL_COMMAND_SOURCE,
    UserCommands,
};
use rust_cli_policy::PolicyGate;

use super::*;
use crate::skills::runner;
use crate::test_support::{CWD, EchoBash, facts, tool_context};

const GREET: &str = "---\ndescription: Greet someone by name\nargument-hint: <name>\narguments: [who]\nallowed-tools: Bash(echo:*)\n---\nSay hello to $ARGUMENTS ($who, first: $0).\nSkill dir: ${CLAUDE_SKILL_DIR}\nInline: !`echo inline-ok`\n";
const HELPER: &str = "---\ndescription: A helper the model must not call\ndisable-model-invocation: true\n---\nHidden helper.\n";
const INNER: &str = "---\ndescription: Model only\nuser-invocable: false\n---\nInner body.\n";
const DENIED: &str = "---\ndescription: Runs a denied command\n---\nOut: !`rm -rf /tmp/x`\n";

fn env() -> Arc<MemoryEnvironment> {
    Arc::new(
        MemoryEnvironment::new(facts())
            .with_file(&format!("{CWD}/.zcode/skills/greet/SKILL.md"), GREET)
            .with_file(&format!("{CWD}/.zcode/skills/helper/SKILL.md"), HELPER)
            .with_file(&format!("{CWD}/.zcode/skills/inner/SKILL.md"), INNER)
            .with_file(&format!("{CWD}/.zcode/skills/denied/SKILL.md"), DENIED)
            .with_file(
                &format!("{CWD}/.zcode/skills/plain/SKILL.md"),
                "# Plain skill\n\nNo frontmatter here.\n",
            ),
    )
}

fn commands(env: Arc<MemoryEnvironment>) -> SkillCommands {
    let catalog = Arc::new(SkillCatalog::new(env.clone(), env));
    let ids: Arc<dyn Ids> = Arc::new(SequentialIds::default());
    let bash = Arc::new(EchoBash::default());
    SkillCommands::new(
        catalog,
        Some(runner(bash, Arc::new(PolicyGate::default()), ids)),
    )
}

fn input(text: &str) -> ExpandedMessage {
    ExpandedMessage {
        text: text.to_owned(),
        kind: ExpandedKind::Input,
    }
}

fn meta(source: &str, text: &str) -> ExpandedMessage {
    ExpandedMessage {
        text: text.to_owned(),
        kind: ExpandedKind::Meta {
            source: source.to_owned(),
        },
    }
}

async fn expand(text: &str) -> Expansion {
    let env = env();
    commands(env.clone())
        .expand(text, tool_context(env, PermissionMode::DontAsk))
        .await
}

const CAVEAT: &str = "<local-command-caveat>The command below was run directly in ZCode, not sent to you as a request, and its output goes straight to the user. It's recorded here as context for later messages.</local-command-caveat>";

#[tokio::test]
async fn k6_greet_expands_to_tags_and_the_body() {
    assert_eq!(
        // EchoBash 的输出是 `out:<命令>`（替身）。
        expand("  /greet Ada  ").await,
        Expansion::Messages {
            messages: vec![
                input(
                    "<command-message>greet</command-message>\n<command-name>/greet</command-name>\n<command-args>Ada</command-args>"
                ),
                meta(
                    SKILL_MESSAGE_SOURCE,
                    &format!(
                        "Base directory for this skill: {CWD}/.zcode/skills/greet\n\nSay hello to Ada (Ada, first: Ada).\nSkill dir: {CWD}/.zcode/skills/greet\nInline: out:echo inline-ok\n"
                    ),
                ),
            ],
            query: true,
        }
    );
}

#[tokio::test]
async fn k7_disabled_for_the_model_still_expands_without_args() {
    assert_eq!(
        expand("/helper").await,
        Expansion::Messages {
            messages: vec![
                input(
                    "<command-message>helper</command-message>\n<command-name>/helper</command-name>"
                ),
                meta(
                    SKILL_MESSAGE_SOURCE,
                    &format!(
                        "Base directory for this skill: {CWD}/.zcode/skills/helper\n\nHidden helper.\n"
                    ),
                ),
            ],
            query: true,
        }
    );
    // 没有占位符时参数追加在末尾（与 Skill 工具相同，K1 的 plain）。
    let Expansion::Messages { messages, .. } = expand("/plain extra words").await else {
        panic!("plain should expand");
    };
    assert!(
        messages[1]
            .text
            .ends_with("No frontmatter here.\n\n\nARGUMENTS: extra words")
    );
}

#[tokio::test]
async fn k8_builtin_loop_expands_to_its_prompt() {
    let Expansion::Messages { messages, query } = expand("/loop 5m check the deploy").await else {
        panic!("/loop should expand");
    };
    assert!(query);
    assert_eq!(
        messages[0],
        input(
            "<command-message>loop</command-message>\n<command-name>/loop</command-name>\n<command-args>5m check the deploy</command-args>"
        )
    );
    assert!(messages[1].text.contains("5m check the deploy"));
    assert!(!messages[1].text.starts_with("Base directory"));
}

#[tokio::test]
async fn k9_unknown_names_and_paths_are_plain_input() {
    for text in ["/nope foo", "/tmp/notes.md", "hello /greet", "/", "/ greet"] {
        assert_eq!(expand(text).await, Expansion::NotCommand, "{text}");
    }
}

#[tokio::test]
async fn not_user_invocable_answers_locally_without_a_query() {
    assert_eq!(
        expand("/inner please").await,
        Expansion::Messages {
            messages: vec![
                meta(LOCAL_COMMAND_CAVEAT_SOURCE, CAVEAT),
                input("/inner"),
                meta(
                    LOCAL_COMMAND_SOURCE,
                    "This skill can only be invoked by ZCode, not directly by users. Ask ZCode to use the \"inner\" skill for you.",
                ),
            ],
            query: false,
        }
    );
}

#[tokio::test]
async fn a_denied_inline_command_is_reported_without_a_query() {
    // dontAsk 下没有 allow 规则：`!` 命令的判决失败（命令输入与 stderr 的消息）。
    let Expansion::Messages { messages, query } = expand("/denied now").await else {
        panic!("denied should expand to an error");
    };
    assert!(!query);
    assert_eq!(messages[0], meta(LOCAL_COMMAND_CAVEAT_SOURCE, CAVEAT));
    assert_eq!(
        messages[1],
        input(
            "<command-name>/denied</command-name>\n            <command-message>denied</command-message>\n            <command-args>now</command-args>"
        )
    );
    let ExpandedKind::Meta { source } = &messages[2].kind else {
        panic!("the error is a meta message");
    };
    assert_eq!(source, LOCAL_COMMAND_SOURCE);
    assert!(
        messages[2]
            .text
            .starts_with("<local-command-stderr>Shell command permission check failed for pattern \"!`rm -rf /tmp/x`\": "),
        "{}",
        messages[2].text
    );
    assert!(messages[2].text.ends_with("</local-command-stderr>"));
}

#[tokio::test]
async fn completion_lists_user_invocable_skills_in_catalog_order() {
    let env = env();
    let listed = commands(env)
        .commands(&crate::test_support::workspace())
        .await;
    let names: Vec<&str> = listed
        .iter()
        .filter(|command| !command.hidden)
        .map(|command| command.name.as_str())
        .collect();
    // user-invocable: false 的 inner 标为 hidden（不进补全，输入时仍是命令）；禁止模型调用的 helper 照列；
    // 内置 loop 与内置提示词命令（修复线 L-A）在文件 skill 之后。
    assert_eq!(
        names,
        [
            "denied",
            "greet",
            "helper",
            "plain",
            "loop",
            "init",
            "security-review"
        ]
    );
    assert!(
        listed
            .iter()
            .any(|command| command.name == "inner" && command.hidden)
    );
    assert_eq!(listed[1].description, "Greet someone by name");
    assert_eq!(listed[1].argument_hint.as_deref(), Some("<name>"));
}

#[test]
fn slash_input_is_parsed() {
    assert_eq!(
        parse("/greet  Ada  Lovelace "),
        Some(("greet", "Ada  Lovelace"))
    );
    assert_eq!(parse("\n/greet"), Some(("greet", "")));
    assert_eq!(parse("/greet\tAda"), Some(("greet", "Ada")));
    assert_eq!(parse("greet"), None);
    assert_eq!(parse("/"), None);
    assert_eq!(parse("/ x"), None);
}

/// `-p`（单次打印会话）下未知命令：输入照常发送，后面跟 `unknown_command_fallback` 提醒。
#[tokio::test]
async fn k9_print_sessions_add_the_unknown_command_note() {
    let env = env();
    let commands = commands(env.clone());
    commands.catalog.set_print_session(true);
    let expansion = commands
        .expand(
            "/nope foo",
            tool_context(env.clone(), PermissionMode::DontAsk),
        )
        .await;
    let Expansion::Messages { messages, query } = expansion else {
        panic!("a print session adds the note");
    };
    assert!(query);
    assert_eq!(messages[0], input("/nope foo"));
    assert_eq!(
        messages[1].kind,
        ExpandedKind::Attachment {
            source: "unknown_command_fallback".into()
        }
    );
    // 列出模型能用的 skill（helper 禁止模型调用、inner 不可由用户调用，都不列）；总数是可输入的 skill。
    assert_eq!(
        messages[1].text,
        "<system-reminder>\nThe user's message starts with the slash command /nope, but no command with that name is available in this session, so it did not run. Slash commands available in this session: /denied, /greet, /plain, /loop, /init, /security-review, and 1 more. Treat the message as a plain request and do the task with the tools you have. If the task needs that command, tell the user it is not installed in this session. The user can add it as an organization plugin or a project skill. Do not give installation steps you are not sure of. Do not say the command ran.\n</system-reminder>"
    );
    // 近似的名字给出候选。
    let Expansion::Messages { messages, .. } = commands
        .expand("/gret", tool_context(env.clone(), PermissionMode::DontAsk))
        .await
    else {
        panic!("note expected");
    };
    assert!(
        messages[1]
            .text
            .contains("The closest available command is /greet. Ask the user before running it.")
    );
    // 路径（`/` 开头、执行环境里存在）与不像命令名的输入不加提醒。
    for text in ["/tmp/notes.md", "/Users rest"] {
        let env_with_dir = Arc::new(MemoryEnvironment::new(facts()).with_file("/Users/dev/x", "x"));
        let commands = super::SkillCommands::new(
            Arc::new(SkillCatalog::new(
                env_with_dir.clone(),
                env_with_dir.clone(),
            )),
            None,
        );
        commands.catalog.set_print_session(true);
        assert_eq!(
            commands
                .expand(text, tool_context(env_with_dir, PermissionMode::DontAsk))
                .await,
            Expansion::NotCommand,
            "{text}"
        );
    }
}
