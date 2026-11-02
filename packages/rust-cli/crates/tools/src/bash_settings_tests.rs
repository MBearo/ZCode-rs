use super::*;

/// 渲染改动的每个锚点在数据文件里恰好出现一次：找不到时以前的替换静默不改（审查 T12），现在这里失败。
#[test]
fn every_anchor_occurs_once_in_the_data_file() {
    let data = rust_cli_prompts::definition(crate::bash::NAME);
    let base = BashTimeouts::default();
    let once = |text: &str, anchor: &str| {
        assert_eq!(text.matches(anchor).count(), 1, "{anchor:?}");
    };
    let description = data.description.as_str();
    for anchor in [
        base.foreground_line(),
        base.deadline_sentence(),
        AVOID_LINE.to_owned(),
        MONITOR_ANCHOR.to_owned(),
    ] {
        once(description, &anchor);
    }
    let field = |pointer: &str| {
        data.schema
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap()
            .to_owned()
    };
    once(&field(TIMEOUT_POINTER), &base.timeout_parameter());
    once(&field(BACKGROUND_POINTER), &base.deadline_parameter());
}

/// 缺省设置渲染出的就是数据文件的文字。
#[test]
fn the_default_settings_render_the_data_file() {
    let data = rust_cli_prompts::definition(crate::bash::NAME);
    let rendered = BashSettings::default().render();
    assert_eq!(rendered.description, data.description);
    assert_eq!(rendered.schema, data.schema);
}

/// 设置之间互不影响：每一项只改它自己的那几处（Monitor 的一句接在期限之后或去掉期限之后都对）。
#[test]
fn each_setting_changes_only_its_own_text() {
    let timeouts = BashTimeouts::new(Some(900_000), Some(8_000_000));
    let all = BashSettings {
        timeouts,
        background_deadline: false,
        bash_first_trimmed: true,
        monitor: true,
    }
    .render();
    let description = all.description.as_str();
    assert!(description.contains("default 900000, max 8000000 for a foreground command."));
    assert!(!description.contains("With it, `timeout`"));
    assert!(!description.contains("Avoid using this tool to run"));
    assert!(description.contains(&format!(
        "re-invokes you when it exits. {MONITOR_ANCHOR}{}",
        rust_cli_prompts::monitor::bash_sentence()
    )));
    assert_eq!(
        all.schema.pointer(TIMEOUT_POINTER).unwrap(),
        "Optional timeout in milliseconds (max 8000000 for a foreground command)"
    );
    assert_eq!(
        all.schema.pointer(BACKGROUND_POINTER).unwrap(),
        "Set to true to run this command in the background."
    );
}
