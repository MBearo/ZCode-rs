use super::*;

/// 收拢前 hooks 与 tools 的任务 ID（逐字照抄，去掉前缀）。
fn old_id(text: &str) -> String {
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut id = String::new();
    for _ in 0..8 {
        id.push(char::from(ALPHABET[(hash % 36) as usize]));
        hash /= 36;
    }
    id
}

#[test]
fn fnv1a_matches_the_published_vectors() {
    assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
}

#[test]
fn short_ids_match_the_old_task_ids() {
    for text in [
        "",
        "op_1",
        "01JABCDEF",
        "operation-with-a-long-name-0123456789",
        "中文",
    ] {
        assert_eq!(short_id(text), old_id(text), "{text}");
        assert_eq!(short_id(text).len(), SHORT_ID_LEN);
    }
}

#[test]
fn hex_and_sha256_hex_match_the_published_vectors() {
    assert_eq!(hex(&[]), "");
    assert_eq!(hex(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
