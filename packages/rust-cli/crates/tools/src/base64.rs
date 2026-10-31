//! 标准 base64 编码（带填充），只用于把图片内联进 tool_result。

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
/// 每 3 个字节编成 4 个字符，每个字符 6 位。
const GROUP_BYTES: usize = 3;
const GROUP_CHARS: usize = 4;
const SEXTET_SHIFTS: [u32; GROUP_CHARS] = [18, 12, 6, 0];
const SEXTET_MASK: u32 = 0x3f;

pub(crate) fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(GROUP_BYTES) * GROUP_CHARS);
    for chunk in bytes.chunks(GROUP_BYTES) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (index, shift) in SEXTET_SHIFTS.into_iter().enumerate() {
            if index <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> shift) & SEXTET_MASK) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn encodes_with_padding() {
        assert_eq!(super::encode(b""), "");
        assert_eq!(super::encode(b"f"), "Zg==");
        assert_eq!(super::encode(b"fo"), "Zm8=");
        assert_eq!(super::encode(b"foo"), "Zm9v");
        assert_eq!(super::encode(&[0xff, 0xfe]), "//4=");
    }
}
