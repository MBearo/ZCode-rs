use super::*;

#[test]
fn every_registered_file_parses() {
    assert_eq!(tools().len(), TOOL_FILES.len());
}

#[test]
fn texts_use_the_product_names() {
    // cron 工具里指产品的地方写 ZCode（tools-more.md §10.2）。
    let cron = tool("CronCreate").unwrap();
    let text = format!("{}{}", cron.description, cron.input_schema);
    for phrase in ["this ZCode session", "when ZCode exits"] {
        assert!(text.contains(phrase), "{phrase}");
    }
    let read = tool("Read").unwrap();
    assert!(
        read.description
            .starts_with("Reads a file from the local filesystem.")
    );
    assert_eq!(read.input_schema["required"][0], "file_path");
}

#[test]
fn every_text_file_is_present() {
    for (path, text) in texts::ALL {
        assert!(!text().is_empty(), "{path}");
    }
}

#[test]
fn templates_render_once_and_keep_unknown_variables() {
    assert_eq!(
        texts::render("a {{x}} b {{y}}", &[("x", "{{y}}"), ("y", "2")]),
        "a {{y}} b 2"
    );
    assert_eq!(texts::render("{{missing}} {{x", &[]), "{{missing}} {{x");
    assert_eq!(texts::variables(texts::model()), ["name", "id"]);
    assert_eq!(
        texts::variables(texts::lean_body()),
        ["intro", "security", "reminders"]
    );
}

#[test]
fn the_identity_is_the_zcode_sentence() {
    assert_eq!(
        texts::identity(),
        "You are ZCode, an interactive coding agent."
    );
}

#[test]
fn model_cards_strip_the_context_suffix() {
    let card = models::card("claude-opus-5-5[1m]").unwrap();
    assert_eq!(card.marketing_name.as_deref(), Some("Opus 5.5"));
    assert_eq!(card.max_output_tokens, 128_000);
    assert!(models::card("glm-5").is_none());
}

/// R1-16：ToolSearch 是否启用只有这一个判定（卡片的 `tool_search`；没有卡片的模型不支持）。
#[test]
fn tool_search_support_comes_from_the_card() {
    assert!(models::supports_tool_search("claude-opus-5-5[1m]", None));
    assert!(!models::supports_tool_search("glm-5", None));
    // 端点的能力答得出时只看它（工作线 F52）。
    assert!(!models::supports_tool_search(
        "claude-opus-5-5",
        Some(false)
    ));
    assert!(models::supports_tool_search("glm-5", Some(true)));
}

#[test]
fn reminders_wrap_and_unwrap() {
    let wrapped = reminder::wrap("hello");
    assert_eq!(wrapped, "<system-reminder>\nhello\n</system-reminder>");
    assert_eq!(reminder::unwrap(&wrapped), "hello");
    assert_eq!(reminder::unwrap("plain"), "plain");
    assert!(reminder::is_wrapped(&wrapped));
}

/// 会话词组的三张词表（tools-more.md §11.6），含表里的重复项。
#[test]
fn the_slug_word_lists_keep_their_sizes_and_order() {
    let words = slug_words();
    assert_eq!(
        (words.adjectives.len(), words.verbs.len(), words.nouns.len()),
        (219, 109, 409)
    );
    assert_eq!(words.adjectives[0], "abundant");
    assert_eq!(words.verbs[0], "baking");
    assert_eq!(words.nouns[0], "aurora");
    assert_eq!(words.nouns[408], "yao");
}

/// 修复线 F51（context.md §6、agents.md §2）：没有环境段，Agent 没有 `model` 参数。别的产品与模型厂商的说法由导出前的
/// 词表扫描拦下（export.md §4），这里不再逐个断言。
#[test]
fn texts_have_no_environment_section_or_model_parameter() {
    let agent = tool("Agent").unwrap();
    assert!(agent.input_schema["properties"].get("model").is_none());
    assert!(agent.input_schema["properties"].get("effort").is_some());
    // 修复线 F51b（agents.md §2）：没有 isolation 参数与说明它的一行，不提 SDK 的 `agents` 选项。
    assert!(agent.input_schema["properties"].get("isolation").is_none());
    assert!(!agent.description.contains("isolation"));
    assert!(!agent.description.contains("SDK"));
    // 修复线 F51c（tools-more.md §11.1）：EnterWorktree 不提子代理的 isolation。
    assert!(
        !tool("EnterWorktree")
            .unwrap()
            .description
            .contains("subagent isolation")
    );
    for (path, text) in texts::ALL {
        assert!(
            !text().starts_with("# Environment\n - The most recent"),
            "{path}"
        );
    }
}

#[test]
fn data_files_lose_exactly_one_trailing_newline() {
    // 以前 7 个模块各写一份、commands 内联一处，写法相同（F58）。
    assert_eq!(trim("a\n\n"), "a\n");
    assert_eq!(trim("a\n"), "a");
    assert_eq!(trim("a"), "a");
    assert_eq!(trim(""), "");
}
