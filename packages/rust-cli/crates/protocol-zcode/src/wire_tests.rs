use base64::Engine;
use serde_json::json;

use super::*;

#[test]
fn the_checksum_is_the_ieee_crc32() {
    assert_eq!(crc32(b""), "00000000");
    assert_eq!(crc32(b"123456789"), "cbf43926");
}

#[test]
fn small_frames_go_out_whole() {
    let frame = json!({"topic": "t/1", "subscriptionId": "s1", "fromSeq": 0, "toSeq": 1});
    let wire = encode(
        &frame,
        &Envelope {
            topic: "t/1",
            subscription: "s1",
            ordinal: 3,
            purpose: Purpose::Online,
        },
    )
    .unwrap();
    assert_eq!(
        wire,
        vec![json!({
            "wireVersion": 3, "kind": "complete", "deliveryKind": "online",
            "logicalFrameId": "s1-lf-3", "logicalFrameOrdinal": 3,
            "topic": "t/1", "subscriptionId": "s1", "frame": frame,
        })]
    );
}

#[test]
fn large_frames_are_split_into_fragments_that_reassemble() {
    let frame = json!({"text": "汉".repeat(400_000)});
    let wire = encode(
        &frame,
        &Envelope {
            topic: "t/1",
            subscription: "s1",
            ordinal: 1,
            purpose: Purpose::Initial,
        },
    )
    .unwrap();
    assert!(wire.len() > 1);
    let mut bytes = Vec::new();
    for (index, fragment) in wire.iter().enumerate() {
        assert_eq!(fragment["kind"], "fragment");
        assert_eq!(fragment["fragmentIndex"], index);
        assert_eq!(fragment["fragmentCount"], wire.len());
        assert!(fragment.to_string().len() < 1024 * 1024);
        bytes.extend(
            base64::engine::general_purpose::STANDARD
                .decode(fragment["dataBase64"].as_str().unwrap())
                .unwrap(),
        );
    }
    assert_eq!(wire[0]["logicalBytes"], bytes.len());
    assert_eq!(wire[0]["checksum"]["value"], crc32(&bytes));
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), frame);
}
