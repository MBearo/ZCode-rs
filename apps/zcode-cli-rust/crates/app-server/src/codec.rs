//! V4 topic frame physical encoding, ported from Node
//! `packages/shared/src/zcode-protocol-v4/wire-codec.ts`.
//!
//! A logical frame is sent as one `complete` notification when every carrier
//! (CLI NDJSON, Channel socket, mobile relay base64 envelope) fits the physical
//! limit; otherwise its UTF-8 bytes are split into checksummed fragments. The
//! logical frame JSON is serialized once by the caller and reused here.
use anyhow::{Result, bail};
use base64::Engine as _;
use std::sync::OnceLock;

pub const WIRE_VERSION: u32 = 3;
/// `PROTOCOL_V4_LIMITS.maxFrameBytes`.
pub const MAX_PHYSICAL_FRAME_BYTES: usize = 1024 * 1024;
/// `PROTOCOL_V4_LIMITS.logicalFrameAssemblyMaxBytes`.
pub const MAX_ASSEMBLY_BYTES: usize = 16 * 1024 * 1024;
/// `PROTOCOL_V4_LIMITS.logicalFrameAssemblyMaxFragments`.
pub const MAX_FRAGMENTS: usize = 1024;
const TRANSPORT_ENVELOPE_ID_MAX_CHARS: usize = 256;
const CHANNEL_EVENT_RESPONSE_TYPE: usize = 204;
const SOCKET_PROTOCOL_HEADER_BYTES: usize = 13;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const NOTIFICATION_PREFIX: &str = r#"{"method":"v4/conversation/frame","params":"#;

/// Identity of one logical frame on one subscription.
pub struct FrameHeader<'a> {
    pub delivery_kind: &'a str,
    pub logical_frame_id: &'a str,
    pub logical_frame_ordinal: u64,
    pub topic: &'a str,
    pub subscription_id: &'a str,
}

fn vql_byte_length(value: usize) -> usize {
    let mut bytes = 1;
    let mut remaining = value >> 7;
    while remaining > 0 {
        bytes += 1;
        remaining >>= 7;
    }
    bytes
}

fn channel_header_bytes() -> usize {
    let max_event_id_json_bytes = MAX_SAFE_INTEGER.to_string().len();
    let max_event_id_serialized =
        1 + vql_byte_length(max_event_id_json_bytes) + max_event_id_json_bytes;
    1 + vql_byte_length(2)
        + 1
        + vql_byte_length(CHANNEL_EVENT_RESPONSE_TYPE)
        + max_event_id_serialized
}

fn mobile_relay_fixed_bytes() -> usize {
    static BYTES: OnceLock<usize> = OnceLock::new();
    *BYTES.get_or_init(|| {
        let transport_id = "x".repeat(TRANSPORT_ENVELOPE_ID_MAX_CHARS);
        serde_json::json!({
            "type":"data",
            "payload":{"zcode_type":"rpc-frame","bridgeSessionId":transport_id,"bridgeGeneration":MAX_SAFE_INTEGER,
                "recoveryId":transport_id,"seq":MAX_SAFE_INTEGER,"dataBase64":""},
            "client_ts":MAX_SAFE_INTEGER,"server_ts":MAX_SAFE_INTEGER,
        })
        .to_string()
        .len()
    })
}

/// Largest physical size of `wire` (serialized topic wire JSON) over the three production carriers.
pub fn measure(wire: &str) -> usize {
    let cli_ndjson = NOTIFICATION_PREFIX.len() + wire.len() + 1 + 1;
    let channel_payload = channel_header_bytes() + 1 + vql_byte_length(wire.len()) + wire.len();
    let channel_socket = channel_payload + SOCKET_PROTOCOL_HEADER_BYTES;
    let mobile_relay = mobile_relay_fixed_bytes() + 4 * channel_payload.div_ceil(3);
    cli_ndjson.max(channel_socket).max(mobile_relay)
}

fn quote(value: &str) -> String {
    serde_json::to_string(value).expect("string serialization is infallible")
}

fn complete_wire(header: &FrameHeader<'_>, frame_json: &str) -> String {
    format!(
        r#"{{"wireVersion":{WIRE_VERSION},"kind":"complete","deliveryKind":{},"logicalFrameId":{},"logicalFrameOrdinal":{},"topic":{},"subscriptionId":{},"frame":{frame_json}}}"#,
        quote(header.delivery_kind),
        quote(header.logical_frame_id),
        header.logical_frame_ordinal,
        quote(header.topic),
        quote(header.subscription_id),
    )
}

struct Fragment<'a> {
    index: usize,
    count: usize,
    logical_bytes: usize,
    checksum: &'a str,
    data_base64: &'a str,
}

fn fragment_wire(header: &FrameHeader<'_>, fragment: &Fragment<'_>) -> String {
    format!(
        r#"{{"wireVersion":{WIRE_VERSION},"kind":"fragment","deliveryKind":{},"logicalFrameId":{},"logicalFrameOrdinal":{},"topic":{},"subscriptionId":{},"fragmentIndex":{},"fragmentCount":{},"logicalBytes":{},"checksum":{{"algorithm":"crc32","value":{}}},"dataBase64":{}}}"#,
        quote(header.delivery_kind),
        quote(header.logical_frame_id),
        header.logical_frame_ordinal,
        quote(header.topic),
        quote(header.subscription_id),
        fragment.index,
        fragment.count,
        fragment.logical_bytes,
        quote(fragment.checksum),
        quote(fragment.data_base64),
    )
}

fn notification(wire: &str) -> String {
    format!("{NOTIFICATION_PREFIX}{wire}}}")
}

/// Encode one logical frame into NDJSON lines (without trailing newline).
pub fn encode(header: &FrameHeader<'_>, frame_json: &str) -> Result<Vec<String>> {
    encode_with_limit(header, frame_json, MAX_PHYSICAL_FRAME_BYTES)
}

fn encode_with_limit(
    header: &FrameHeader<'_>,
    frame_json: &str,
    max_physical: usize,
) -> Result<Vec<String>> {
    let logical = frame_json.as_bytes();
    if logical.len() > MAX_ASSEMBLY_BYTES {
        bail!("proto.frameAssemblyTooLarge");
    }
    let complete = complete_wire(header, frame_json);
    if measure(&complete) <= max_physical {
        return Ok(vec![notification(&complete)]);
    }
    let checksum = format!("{:08x}", crc32fast::hash(logical));
    let chunk = fragment_budget(header, logical.len(), &checksum, max_physical);
    if chunk < 1 {
        bail!("proto.frameEnvelopeTooLarge");
    }
    let count = logical.len().div_ceil(chunk);
    if count > MAX_FRAGMENTS {
        bail!("proto.frameFragmentCountExceeded");
    }
    let engine = base64::engine::general_purpose::STANDARD;
    let mut lines = Vec::with_capacity(count);
    for (index, bytes) in logical.chunks(chunk).enumerate() {
        let data_base64 = engine.encode(bytes);
        let wire = fragment_wire(
            header,
            &Fragment {
                index,
                count,
                logical_bytes: logical.len(),
                checksum: &checksum,
                data_base64: &data_base64,
            },
        );
        // 估算与真实信封不一致时必须 fail closed，不能把超限帧交给下游截断。
        if measure(&wire) > max_physical {
            bail!("proto.frameEnvelopeTooLarge");
        }
        lines.push(notification(&wire));
    }
    Ok(lines)
}

/// Largest chunk size whose worst-case fragment envelope fits, found by binary search
/// with the widest possible index digits (same as Node `findFragmentByteBudget`).
fn fragment_budget(
    header: &FrameHeader<'_>,
    logical_bytes: usize,
    checksum: &str,
    max_physical: usize,
) -> usize {
    let (mut low, mut high, mut best) = (1, logical_bytes.min(max_physical), 0);
    let worst_count = logical_bytes;
    while low <= high {
        let candidate = (low + high) / 2;
        let data_base64 = "A".repeat(4 * candidate.div_ceil(3));
        let wire = fragment_wire(
            header,
            &Fragment {
                index: worst_count.saturating_sub(1),
                count: worst_count,
                logical_bytes,
                checksum,
                data_base64: &data_base64,
            },
        );
        if measure(&wire) <= max_physical {
            best = candidate;
            low = candidate + 1;
        } else {
            high = candidate - 1;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn header() -> FrameHeader<'static> {
        FrameHeader {
            delivery_kind: "online",
            logical_frame_id: "sub-1-lf-1",
            logical_frame_ordinal: 1,
            topic: "conversation/s",
            subscription_id: "sub-1",
        }
    }

    fn frame_json(payload_bytes: usize) -> String {
        serde_json::json!({"topic":"conversation/s","payload":{"kind":"deltas","text":"é".repeat(payload_bytes / 2)}})
            .to_string()
    }

    #[test]
    fn small_frames_stay_complete_and_parse() {
        let lines = encode(&header(), &frame_json(64)).unwrap();
        assert_eq!(lines.len(), 1);
        let line: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(line["params"]["kind"], "complete");
        assert_eq!(line["params"]["frame"]["topic"], "conversation/s");
    }

    #[test]
    fn every_physical_frame_fits_all_carriers_and_reassembles() {
        // 900 KiB 的逻辑帧在 CLI 行内可放下，但 relay base64 后超过 1 MiB，必须分片。
        let frame = frame_json(900 * 1024);
        let lines = encode(&header(), &frame).unwrap();
        assert!(lines.len() > 1);
        let mut data = Vec::new();
        for line in &lines {
            let value: Value = serde_json::from_str(line).unwrap();
            let wire = value["params"].to_string();
            assert!(measure(&wire) <= MAX_PHYSICAL_FRAME_BYTES);
            assert_eq!(value["params"]["kind"], "fragment");
            data.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(value["params"]["dataBase64"].as_str().unwrap())
                    .unwrap(),
            );
        }
        assert_eq!(data, frame.as_bytes());
        let first: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(
            first["params"]["checksum"]["value"],
            format!("{:08x}", crc32fast::hash(frame.as_bytes()))
        );
    }

    #[test]
    fn oversized_logical_frames_fail_closed() {
        let frame = "x".repeat(MAX_ASSEMBLY_BYTES + 1);
        assert_eq!(
            encode(&header(), &frame).unwrap_err().to_string(),
            "proto.frameAssemblyTooLarge"
        );
        assert_eq!(
            encode_with_limit(&header(), &frame_json(4096), 200)
                .unwrap_err()
                .to_string(),
            "proto.frameEnvelopeTooLarge"
        );
    }

    #[test]
    fn measurement_matches_node_constants() {
        // 1 + vql(2) + 1 + vql(204) + (1 + vql(16) + 16)
        assert_eq!(channel_header_bytes(), 23);
        let wire = "{}";
        assert_eq!(
            measure(wire),
            (mobile_relay_fixed_bytes() + 4 * (23 + 1 + 1 + 2usize).div_ceil(3))
                .max(NOTIFICATION_PREFIX.len() + 2 + 2)
        );
    }
}
