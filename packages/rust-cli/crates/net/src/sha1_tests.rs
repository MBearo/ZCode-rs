//! SHA-1：FIPS 180-2 附录的测试向量（一块与两块的填充都走到）。

use super::digest;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn digests_match_the_standard_vectors() {
    assert_eq!(
        hex(&digest(b"")),
        "da39a3ee5e6b4b0d3255bfef95601890afd80709"
    );
    assert_eq!(
        hex(&digest(b"abc")),
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    // 56 字节：长度字段放不进第一块，填充出第二块。
    assert_eq!(
        hex(&digest(
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
        )),
        "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
    );
    // 多块的消息。
    assert_eq!(
        hex(&digest(&[b'a'; 1000])),
        "291e9a6c66994949b57ba5e650361e98fc36b1ba"
    );
}
