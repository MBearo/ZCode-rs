//! App 协议的物理帧 wire v3（`packages/shared/src/zcode-protocol-v4/wire.ts`、`wire-codec.ts`）：逻辑帧放进 `complete`
//! 帧；太大的逻辑帧按 UTF-8 字节分片（`fragment`，整段逻辑帧的 crc32、每片 base64），对端的装配器拼回去。

use base64::Engine;
use rust_cli_base::units;
use serde_json::{Value, json};

const WIRE_VERSION: u64 = 3;
/// 一个 complete 帧的 JSON 的上限（字节）。对端按三条链路里最大的那个量（手机的中继把整行 base64，约 4/3 倍），上限
/// 1 MiB；这里留出余量。
const COMPLETE_LIMIT: usize = 700 * units::KIB;
/// 分片时每片的逻辑字节数（base64 之后约 600 KiB）。
const FRAGMENT_BYTES: usize = 450 * units::KIB;
/// 逻辑帧装配的上限（对端 `logicalFrameAssemblyMaxBytes`）。
const ASSEMBLY_LIMIT: usize = 16 * units::MIB;

/// crc32（IEEE 802.3，反射）的初值与结果的异或值。
const CRC32_INIT: u32 = 0xffff_ffff;
/// crc32（IEEE 802.3）的生成多项式（反射的写法）。
const CRC32_POLYNOMIAL: u32 = 0xedb8_8320;

/// 帧的用途（对端按它区分首帧、在线增量与恢复）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Initial,
    Online,
    Recovery,
}

impl Purpose {
    fn name(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Online => "online",
            Self::Recovery => "recovery",
        }
    }
}

/// 一个逻辑帧在物理帧上的身份：订阅里的第几个。
pub struct Envelope<'a> {
    pub topic: &'a str,
    pub subscription: &'a str,
    pub ordinal: u64,
    pub purpose: Purpose,
}

/// crc32（IEEE，与对端 `crc32WireBytes` 相同），8 位小写十六进制。
pub fn crc32(bytes: &[u8]) -> String {
    let mut crc = CRC32_INIT;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..u8::BITS {
            crc = (crc >> 1) ^ if crc & 1 == 1 { CRC32_POLYNOMIAL } else { 0 };
        }
    }
    format!("{:08x}", crc ^ CRC32_INIT)
}

/// 逻辑帧 → 物理帧（通常一个；太大时多个分片）。超过装配上限时返回 None（调用方改发快照恢复）。
pub fn encode(frame: &Value, envelope: &Envelope<'_>) -> Option<Vec<Value>> {
    let logical_frame_id = format!("{}-lf-{}", envelope.subscription, envelope.ordinal);
    let complete = json!({
        "wireVersion": WIRE_VERSION,
        "kind": "complete",
        "deliveryKind": envelope.purpose.name(),
        "logicalFrameId": logical_frame_id,
        "logicalFrameOrdinal": envelope.ordinal,
        "topic": envelope.topic,
        "subscriptionId": envelope.subscription,
        "frame": frame,
    });
    let logical = frame.to_string();
    if logical.len() > ASSEMBLY_LIMIT {
        return None;
    }
    if complete.to_string().len() <= COMPLETE_LIMIT {
        return Some(vec![complete]);
    }
    let bytes = logical.as_bytes();
    let checksum = crc32(bytes);
    let count = bytes.len().div_ceil(FRAGMENT_BYTES);
    Some(
        bytes
            .chunks(FRAGMENT_BYTES)
            .enumerate()
            .map(|(index, chunk)| {
                json!({
                    "wireVersion": WIRE_VERSION,
                    "kind": "fragment",
                    "deliveryKind": envelope.purpose.name(),
                    "logicalFrameId": logical_frame_id,
                    "logicalFrameOrdinal": envelope.ordinal,
                    "topic": envelope.topic,
                    "subscriptionId": envelope.subscription,
                    "fragmentIndex": index,
                    "fragmentCount": count,
                    "logicalBytes": bytes.len(),
                    "checksum": {"algorithm": "crc32", "value": checksum},
                    "dataBase64": base64::engine::general_purpose::STANDARD.encode(chunk),
                })
            })
            .collect(),
    )
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;
