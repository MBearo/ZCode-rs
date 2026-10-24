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

/// ToolSearch 的形式只有这一个判定（tools-more.md §3.2.2）：端点的能力为真（只有 debug 构建的测试声明这样设）时原生，
/// 其余本地；不看模型。修改（工作线 F89）：F84 时禁用表里的模型不开，端点答不出能力时按卡片回退成原生。
#[test]
fn tool_search_mode_is_local_unless_the_endpoint_is_native() {
    use models::ToolSearchMode::{Local, Native, Off};
    assert_eq!(models::tool_search_mode(Some(true)), Native);
    assert_eq!(models::tool_search_mode(Some(false)), Local);
    assert_eq!(models::tool_search_mode(None), Local);
    assert!(Local.is_on() && Native.is_on() && !Off.is_on());
}

/// `ENABLE_TOOL_SEARCH` 的设置（tools-more.md §3.2.1，工作线 F89）：关以外都乐观地算开；这一步开不开，自动时按延迟工具的
/// 字符数对 `floor(floor(窗口 × N / 100) × 2.5)`，只在自动时才算字符数。
#[test]
fn the_setting_decides_on_off_and_the_auto_threshold() {
    use models::ToolSearchSetting::{Auto, Off, On};
    assert!(On.allows() && Auto { percent: 10 }.allows() && !Off.allows());
    assert_eq!(models::ToolSearchSetting::default(), On);
    let unused = || -> u64 { panic!("only the automatic setting measures the tools") };
    assert!(On.reached(200_000, unused));
    assert!(!Off.reached(200_000, unused));
    // 200,000 的 10% 是 20,000 token，门槛 50,000 个字符。
    let auto = Auto { percent: 10 };
    assert!(!auto.reached(200_000, || 49_999));
    assert!(auto.reached(200_000, || 50_000));
    // 1,000,000 的 1% 是 10,000 token，门槛 25,000；向下取整：101 的 10% 是 10 token，门槛 25。
    assert!(Auto { percent: 1 }.reached(1_000_000, || 25_000));
    assert!(!Auto { percent: 1 }.reached(1_000_000, || 24_999));
    assert!(auto.reached(101, || 25) && !auto.reached(101, || 24));
}

/// 上下文窗口（自动门槛用）：配置的窗口大于 0 时用它，否则卡片，再没有时 200,000（与压缩的回退同一个缺省）。
#[test]
fn the_context_window_comes_from_the_configuration_then_the_card() {
    assert_eq!(models::context_window("glm-5", Some(128_000)), 128_000);
    assert_eq!(
        models::context_window("claude-opus-5-5", Some(0)),
        1_000_000
    );
    assert_eq!(
        models::context_window("claude-opus-5-5[1m]", None),
        1_000_000
    );
    assert_eq!(
        models::context_window("glm-5", None),
        models::DEFAULT_CONTEXT_WINDOW
    );
    assert_eq!(models::DEFAULT_CONTEXT_WINDOW, 200_000);
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
