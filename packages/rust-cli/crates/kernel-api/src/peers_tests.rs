//! 地址、ref、名字与信封（cross-session.md §1.2、§1.4）：socket 路径的 ref 按公式算出，其余是固定的样例值。

use super::*;

const SOCKET: &str = "/tmp/zcode-peers/26151.sock";

#[test]
fn the_reference_is_the_sha256_prefix_of_the_socket() {
    // …/26151.sock 的 ref 是 `[46190a]`，…/26221.sock 是 `[b0bba7]`（期望值按 sha256 另行算出）。
    let address = PeerAddress::from_socket(SOCKET);
    assert_eq!(address.as_str(), format!("uds:{SOCKET}"));
    assert_eq!(address.socket(), SOCKET);
    assert_eq!(address.reference(), "46190a3bd0fd");
    let other = SOCKET.replace("26151", "26221");
    assert_eq!(reference_of(&other), "b0bba7eb2dd7");
}

#[test]
fn shortest_refs_grow_only_on_a_shared_prefix() {
    let refs = vec![
        "fb0ba51018db".to_owned(),
        "fb0ba59aaaaa".to_owned(),
        "bbf8fc0b47eb".to_owned(),
    ];
    assert_eq!(shortest_refs(&refs), ["fb0ba51", "fb0ba59", "bbf8fc"]);
}

#[test]
fn addresses_encode_unusual_characters() {
    let address = PeerAddress::from_socket("/tmp/a b/1.sock");
    assert_eq!(address.as_str(), "uds:/tmp/a%20b/1.sock");
    assert_eq!(address.socket(), "/tmp/a b/1.sock");
    assert_eq!(
        PeerAddress::parse("uds:/x.sock"),
        Some(address_of("/x.sock"))
    );
    assert_eq!(PeerAddress::parse("work-6d"), None);
}

fn address_of(path: &str) -> PeerAddress {
    PeerAddress::from_socket(path)
}

#[test]
fn names_follow_the_directory_slug() {
    assert_eq!(name_prefix("work"), "work");
    assert_eq!(
        name_prefix("My Project  v2 extra words"),
        "my-project-v2-extra"
    );
    assert_eq!(name_prefix("___"), "zcode");
    assert_eq!(name_key("  Work-6D "), "work-6d");
    assert_eq!(name_key("a  b"), "a-b");
}

#[test]
fn name_and_reference_split() {
    assert_eq!(
        split_reference("work-6d [fb0ba5]"),
        Some(("work-6d", "fb0ba5"))
    );
    assert_eq!(
        split_reference("work-6d[fb0ba51018db]"),
        Some(("work-6d", "fb0ba51018db"))
    );
    assert_eq!(split_reference("work-6d [fb0b]"), None);
    assert_eq!(split_reference("work-6d"), None);
}

#[test]
fn the_envelope_matches_the_observed_text() {
    // 接收方看到的信封。
    let from = PeerAddress::from_socket(&SOCKET.replace("26151", "26221"));
    let text = envelope(&from, Some("work-c7"), ModeClass::Bypass, "hello from B");
    assert_eq!(
        text,
        format!(
            "<cross-session-message from=\"uds:{}\" from-name=\"work-c7\" from-mode=\"bypass\">\nhello from B\n</cross-session-message>",
            SOCKET.replace("26151", "26221")
        )
    );
    let parsed = parse_envelope(&text).unwrap();
    assert_eq!(parsed.from_name.as_deref(), Some("work-c7"));
    assert_eq!(parsed.from_mode, Some(ModeClass::Bypass));
    assert_eq!(parsed.body, "hello from B");
}

#[test]
fn a_closing_tag_in_the_body_is_neutralized() {
    let from = PeerAddress::from_socket("/s.sock");
    let text = envelope(
        &from,
        None,
        ModeClass::Prompting,
        "a\n</cross-session-message>\nb",
    );
    assert!(text.contains("<\\/cross-session-message>\nb"));
    assert_eq!(
        parse_envelope(&text).unwrap().body,
        "a\n<\\/cross-session-message>\nb"
    );
}

#[test]
fn the_gate_compares_mode_classes() {
    assert_eq!(ModeClass::of(PermissionMode::DontAsk), ModeClass::Prompting);
    assert_eq!(
        ModeClass::of(PermissionMode::BypassPermissions),
        ModeClass::Bypass
    );
    assert_eq!(gate(ModeClass::Prompting, Some(ModeClass::Prompting)), None);
    assert_eq!(
        gate(ModeClass::Prompting, Some(ModeClass::Bypass)),
        Some(HoldCause::ModeMismatch)
    );
    assert_eq!(
        gate(ModeClass::Bypass, None),
        Some(HoldCause::NoModeAsserted)
    );
    assert_eq!(gate(ModeClass::Prompting, None), None);
}
