//! 旁路文件的读法（remote-exec.md §22.2）：绑定、`local`、坏的项与坏的文件。

use super::*;

#[test]
fn bindings_name_remote_machines_and_skip_local_ones() {
    let text = br#"{"version": 1, "servers": {
        "browser-a": {"machine": "dev-a"},
        "here": {"machine": "local"},
        "blank": {"machine": ""},
        "broken": "dev-b",
        "future": {"machine": "gpu-1", "note": "kept for later"}
    }}"#;
    let (bindings, warnings) = McpBindings::parse(text);
    assert_eq!(bindings.machine("browser-a"), Some("dev-a"));
    assert_eq!(bindings.machine("future"), Some("gpu-1"));
    assert_eq!(bindings.machine("here"), None);
    assert_eq!(bindings.machine("blank"), None);
    assert_eq!(bindings.machine("broken"), None);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].starts_with("servers.broken"));
}

#[test]
fn a_bad_file_means_no_bindings() {
    for text in [&b"not json"[..], br#"{"servers": []}"#, br#"[]"#] {
        let (bindings, warnings) = McpBindings::parse(text);
        assert_eq!(bindings, McpBindings::default());
        assert_eq!(warnings.len(), 1);
    }
}
