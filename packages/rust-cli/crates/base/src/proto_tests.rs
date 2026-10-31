use super::*;

#[test]
fn success_and_redirect_codes() {
    assert!(http::is_success(200) && http::is_success(299));
    assert!(!http::is_success(199) && !http::is_success(300));
    for code in [301, 302, 303, 307, 308] {
        assert!(http::is_redirect(code), "{code}");
    }
    for code in [300, 304, 305, 306, 200] {
        assert!(!http::is_redirect(code), "{code}");
    }
}

#[test]
fn reason_phrases_cover_the_table_and_unknown_codes_are_empty() {
    assert_eq!(http::reason_phrase(200), "OK");
    assert_eq!(http::reason_phrase(418), "I'm a Teapot");
    assert_eq!(http::reason_phrase(504), "Gateway Timeout");
    assert_eq!(http::reason_phrase(299), "");
    let codes: Vec<u16> = http::REASON_PHRASES.iter().map(|(code, _)| *code).collect();
    let mut sorted = codes.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(codes, sorted);
}

#[test]
fn stat_fields_are_counted_after_the_last_parenthesis() {
    // comm 里有空格与括号。
    let stat = "4242 (a b) c)) S 1 4242 4242 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 98765 1000 10";
    assert_eq!(procfs::field(stat, procfs::STATE), Some("S"));
    assert_eq!(procfs::field(stat, procfs::STARTTIME), Some("98765"));
    assert_eq!(procfs::field(stat, 2), None);
    assert_eq!(procfs::field("no parenthesis", procfs::STATE), None);
    assert_eq!(procfs::stat_path(7), "/proc/7/stat");
}

#[test]
fn permission_bits() {
    assert_eq!(mode::PRIVATE_FILE & !mode::PERMISSION_BITS, 0);
    assert_eq!(mode::PRIVATE_DIR, 0o700);
    assert_eq!(mode::EXECUTABLE, 0o755);
}

#[test]
fn client_errors_and_signal_exit_codes() {
    assert!(http::is_client_error(400) && http::is_client_error(499));
    assert!(!http::is_client_error(399) && !http::is_client_error(500));
    assert_eq!(exit::signal_exit(exit::SIGKILL), 137);
    assert_eq!(exit::signal_exit(exit::SIGTERM), 143);
    assert_eq!(exit::signal_exit(exit::SIGHUP), 129);
}
