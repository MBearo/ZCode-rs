use super::*;

#[test]
fn lines_are_numbered_with_a_tab_and_lose_carriage_returns() {
    assert_eq!(number_lines("a\r\nb", 1), "1\ta\n2\tb");
    assert_eq!(number_lines("x", 0), "0\tx");
}

#[test]
fn binary_and_device_paths_are_recognized() {
    assert!(is_binary("/w/app.EXE"));
    assert!(!is_binary("/w/shot.png"));
    assert!(!is_binary("/w/doc.pdf"));
    assert!(!is_binary("/w/main.rs"));
    assert!(is_device("/dev/zero"));
    assert!(is_device("/proc/42/environ"));
    assert!(!is_device("/proc/42/task/1/environ"));
}

#[test]
fn token_estimates_follow_the_extension() {
    assert_eq!(estimate_tokens(&"x".repeat(100), ".json"), 50);
    assert_eq!(estimate_tokens(&"x".repeat(100), ".rs"), 25);
    assert!(check_tokens(&"x".repeat(100_004), ".rs").is_err());
    assert!(check_tokens(&"x".repeat(100_000), ".rs").is_ok());
}

/// 文件头的判断（修复线 F58，kernel-api `image::media_type`，TUI 的粘贴也用它）：GIF 要 `GIF87a` / `GIF89a`，
/// 不足 4 字节的不认。以前 `GIF8` 开头就算 GIF。
#[test]
fn image_headers_need_the_full_signature() {
    assert_eq!(image_type(b"GIF89a.."), Some("image/gif"));
    assert_eq!(image_type(b"GIF8xa.."), None);
    assert_eq!(image_type(&[0xff, 0xd8, 0xff]), None);
    assert_eq!(image_type(b"\x89PNG"), Some("image/png"));
}
