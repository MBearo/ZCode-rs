use super::*;

#[test]
fn bytes_are_formatted_with_binary_units() {
    assert_eq!(format_bytes(512), "512 bytes");
    assert_eq!(format_bytes(2000), "2KB");
    assert_eq!(format_bytes(262_144), "256KB");
    assert_eq!(format_bytes(297_000), "290KB");
    assert_eq!(format_bytes(99_738), "97.4KB");
    assert_eq!(format_bytes(1_280), "1.3KB");
    assert_eq!(format_bytes(1_073_741_824), "1GB");
}

#[test]
fn control_characters_become_entities() {
    assert_eq!(escape_control("a\u{1b}b\nc"), "a&#27;b&#10;c");
    assert_eq!(escape_control("plain/path"), "plain/path");
}

#[test]
fn every_file_tool_has_its_prompt_data() {
    for name in ["Read", "Write", "Edit", "Glob", "Grep", "NotebookEdit"] {
        let found = definition(name);
        assert!(!found.description.is_empty(), "{name}");
        assert_eq!(found.schema["type"], "object", "{name}");
    }
}
