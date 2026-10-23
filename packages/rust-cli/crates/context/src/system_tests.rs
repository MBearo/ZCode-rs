use super::*;
use crate::config::{OutputStyle, Surface};

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

fn facts<'a>(tool_names: &'a [String], surface: Surface) -> PromptFacts<'a> {
    PromptFacts {
        surface,
        system_turns: true,
        tool_names,
        has_skills: true,
        memory_dir: Some("/m/"),
        output_style: None,
        custom: None,
        append: None,
    }
}

/// `--append-system-prompt`（cli-args.md §4.2）：接在会话段最后，空行连接；块数与缓存断点不变。
#[test]
fn the_appended_prompt_ends_the_session_block() {
    let tools = names(&["Skill"]);
    let plain = system_blocks(&facts(&tools, Surface::Print));
    let mut appended = facts(&tools, Surface::Print);
    appended.append = Some("Always end with DONE.");
    let blocks = system_blocks(&appended);
    assert_eq!(blocks.len(), 3);
    assert_eq!(blocks[1], plain[1]);
    assert_eq!(
        blocks[2].text,
        format!("{}\n\nAlways end with DONE.", plain[2].text)
    );
    assert_eq!(blocks[2].cache_control, plain[2].cache_control);
    assert_eq!(
        sections(&appended).last().map(String::as_str),
        Some("Always end with DONE.")
    );
}

/// `--system-prompt`（cli-args.md §4.2）：缺省段落都不出现；身份句与“自定义 + 追加”两块，都是
/// `{ephemeral}`、没有 scope。
#[test]
fn a_custom_prompt_replaces_the_default_sections() {
    let tools = names(&["Skill"]);
    let mut custom = facts(&tools, Surface::Print);
    custom.custom = Some("You are a terse test assistant.");
    custom.append = Some("Answer in one sentence.");
    let blocks = system_blocks(&custom);
    assert_eq!(blocks.len(), 2);
    assert_eq!(
        blocks[0].text,
        "You are ZCode, an interactive coding agent."
    );
    assert_eq!(
        blocks[1].text,
        "You are a terse test assistant.\n\nAnswer in one sentence."
    );
    for block in &blocks {
        let cache = block.cache_control.as_ref().unwrap();
        assert_eq!(
            (cache.kind.as_str(), cache.scope.as_deref()),
            ("ephemeral", None)
        );
    }
    assert_eq!(
        sections(&custom),
        ["You are a terse test assistant.", "Answer in one sentence."]
    );
    custom.append = None;
    assert_eq!(
        system_blocks(&custom)[1].text,
        "You are a terse test assistant."
    );
}

#[test]
fn print_mode_has_three_blocks_with_their_cache_breakpoints() {
    let tools = names(&["Skill", "Agent"]);
    let blocks = system_blocks(&facts(&tools, Surface::Print));
    assert_eq!(blocks.len(), 3);
    assert_eq!(
        blocks[0].text,
        "You are ZCode, an interactive coding agent."
    );
    assert!(blocks[0].cache_control.is_none());
    let global = blocks[1].cache_control.as_ref().unwrap();
    assert_eq!(
        (global.kind.as_str(), global.scope.as_deref()),
        ("ephemeral", Some("global"))
    );
    assert!(blocks[1].text.contains("mid-conversation system turns"));
    assert_eq!(blocks[2].cache_control.as_ref().unwrap().scope, None);
    let session = &blocks[2].text;
    assert!(session.contains("# Session-specific guidance\n - When the user types"));
    assert!(!session.contains("! <command>"));
    assert!(session.contains("memory at `/m/`"));
    assert!(session.ends_with("<total_tokens>15000000 tokens left</total_tokens>"));
}

#[test]
fn the_terminal_ui_adds_the_shell_hint_and_skills_need_the_skill_tool() {
    let tools = names(&["Agent"]);
    let session = &system_blocks(&facts(&tools, Surface::Interactive))[2].text;
    assert!(session.contains("# Session-specific guidance\n - If you need the user to run"));
    assert!(!session.contains("When the user types"));

    let tools = Vec::new();
    let session = &system_blocks(&facts(&tools, Surface::Print))[2].text;
    assert!(!session.contains("# Session-specific guidance"));
}

#[test]
fn memory_output_style_and_unknown_models_change_their_sections() {
    let tools = Vec::new();
    let style = OutputStyle {
        name: "Explanatory".to_owned(),
        keep_coding_instructions: true,
    };
    let mut facts = facts(&tools, Surface::Print);
    facts.memory_dir = None;
    facts.output_style = Some(&style);
    facts.system_turns = false;
    let blocks = system_blocks(&facts);
    assert!(
        blocks[1]
            .text
            .contains("according to your \"Output Style\"")
    );
    assert!(
        blocks[1]
            .text
            .contains("`<system-reminder>` tags in messages")
    );
    assert!(!blocks[2].text.contains("# Memory"));
}

/// 修复线 F51（context.md §6）：没有环境段，会话段从记忆段直接接到 `# Context management`；`/context` 计数的段落是
/// 同一个列表，也少这一段。
#[test]
fn the_session_block_has_no_environment_section() {
    let tools = names(&["Skill", "Agent"]);
    for surface in [Surface::Print, Surface::Interactive] {
        let facts = facts(&tools, surface);
        let session = &system_blocks(&facts)[2].text;
        assert!(!session.contains("# Environment"), "{session}");
        assert!(session.contains("memory at `/m/`"));
        assert!(session.contains("\n\n# Context management\n"));
        let counted = sections(&facts);
        assert!(
            counted
                .iter()
                .all(|section| !section.starts_with("# Environment")),
            "{counted:?}"
        );
        assert!(
            counted
                .iter()
                .any(|section| section.starts_with("# Context management"))
        );
    }
}
