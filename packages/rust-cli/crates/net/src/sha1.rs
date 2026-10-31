//! SHA-1（只给 WebSocket 握手的 `Sec-WebSocket-Accept` 用，RFC 6455 §4.2.2；不用于安全用途）。
//!
//! 算法常数按 FIPS 180-4 §4.1.1、§4.2.1、§5.1.1、§5.3.1、§6.1。

/// 摘要的字节数。
const DIGEST_BYTES: usize = 20;
/// 初始的散列值 H0..H4（§5.3.1）。
const INITIAL_STATE: [u32; 5] = [
    0x6745_2301,
    0xEFCD_AB89,
    0x98BA_DCFE,
    0x1032_5476,
    0xC3D2_E1F0,
];
/// 一块的字节数。
const BLOCK_BYTES: usize = 64;
/// 一块的字数（消息调度的前 16 个字直接取自这一块）。
const BLOCK_WORDS: usize = BLOCK_BYTES / size_of::<u32>();
/// 消息调度的字数（一块 80 轮，每轮一个字）。
const SCHEDULE_WORDS: usize = 80;
/// 消息调度往回取的四个字的距离：`W[t] = ROTL1(W[t-3] ^ W[t-8] ^ W[t-14] ^ W[t-16])`。
const SCHEDULE_TAPS: [usize; 4] = [3, 8, 14, 16];
/// 填充的第一个字节：一个 1 位，后面补 0（§5.1.1）。
const PADDING_START: u8 = 0x80;
/// 填充到最后一块的这个位置，后面放 8 字节的消息位长（大端）。
const LENGTH_AT: usize = BLOCK_BYTES - size_of::<u64>();
/// 80 轮分四段，每段 20 轮，各用一个逻辑函数与一个轮常数 K。
const ROUNDS_PER_STAGE: usize = 20;
/// 四段的轮常数 K（§4.2.1）。
const ROUND_CONSTANTS: [u32; 4] = [0x5A82_7999, 0x6ED9_EBA1, 0x8F1B_BCDC, 0xCA62_C1D6];
/// 逻辑函数是 Maj 的那一段（第 0 段是 Ch，第 1、3 段是 Parity；§4.1.1）。
const MAJ_STAGE: usize = 2;

/// 20 字节的摘要。
pub(crate) fn digest(data: &[u8]) -> [u8; DIGEST_BYTES] {
    let mut state = INITIAL_STATE;
    let mut message = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(u64::from(u8::BITS));
    message.push(PADDING_START);
    while message.len() % BLOCK_BYTES != LENGTH_AT {
        message.push(0);
    }
    message.extend_from_slice(&bits.to_be_bytes());
    for block in message.chunks(BLOCK_BYTES) {
        let mut words = [0_u32; SCHEDULE_WORDS];
        for (index, word) in block.chunks(size_of::<u32>()).enumerate() {
            words[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for index in BLOCK_WORDS..SCHEDULE_WORDS {
            words[index] = SCHEDULE_TAPS
                .iter()
                .fold(0, |mixed, tap| mixed ^ words[index - tap])
                .rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = state;
        for (index, word) in words.iter().enumerate() {
            let stage = index / ROUNDS_PER_STAGE;
            let f = match stage {
                0 => (b & c) | (!b & d),
                MAJ_STAGE => (b & c) | (b & d) | (c & d),
                _ => b ^ c ^ d,
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(ROUND_CONSTANTS[stage])
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = next;
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e]) {
            *slot = slot.wrapping_add(value);
        }
    }
    let mut out = [0_u8; DIGEST_BYTES];
    for (chunk, word) in out.chunks_mut(size_of::<u32>()).zip(state) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
#[path = "sha1_tests.rs"]
mod tests;
