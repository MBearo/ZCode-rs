//! Node `webfetch-egress-guard.ts`: before every GET, local hostnames and
//! literal non-public IPs are refused. Names are never resolved (D12).
use super::WebError;
use super::url::literal_host;
use url::Url;

/// ipaddr.js 1.9.1 non-`unicast` IPv4 ranges plus the 198.18.0.0/15 benchmark block.
const V4_BLOCKED: [([u8; 4], u32); 16] = [
    ([0, 0, 0, 0], 8),
    ([255, 255, 255, 255], 32),
    ([224, 0, 0, 0], 4),
    ([169, 254, 0, 0], 16),
    ([127, 0, 0, 0], 8),
    ([100, 64, 0, 0], 10),
    ([10, 0, 0, 0], 8),
    ([172, 16, 0, 0], 12),
    ([192, 168, 0, 0], 16),
    ([192, 0, 0, 0], 24),
    ([192, 0, 2, 0], 24),
    ([192, 88, 99, 0], 24),
    ([198, 51, 100, 0], 24),
    ([203, 0, 113, 0], 24),
    ([240, 0, 0, 0], 4),
    ([198, 18, 0, 0], 15),
];

/// ipaddr.js 1.9.1 non-`unicast` IPv6 ranges plus Node's extra special-use list.
const V6_BLOCKED: [([u16; 8], u32); 16] = [
    ([0, 0, 0, 0, 0, 0, 0, 0], 128),
    ([0xfe80, 0, 0, 0, 0, 0, 0, 0], 10),
    ([0xff00, 0, 0, 0, 0, 0, 0, 0], 8),
    ([0, 0, 0, 0, 0, 0, 0, 1], 128),
    ([0xfc00, 0, 0, 0, 0, 0, 0, 0], 7),
    ([0, 0, 0, 0, 0, 0xffff, 0, 0], 96),
    ([0, 0, 0, 0, 0xffff, 0, 0, 0], 96),
    ([0x64, 0xff9b, 0, 0, 0, 0, 0, 0], 96),
    ([0x2002, 0, 0, 0, 0, 0, 0, 0], 16),
    ([0x2001, 0, 0, 0, 0, 0, 0, 0], 32),
    ([0x2001, 0xdb8, 0, 0, 0, 0, 0, 0], 32),
    ([0x64, 0xff9b, 1, 0, 0, 0, 0, 0], 48),
    ([0x100, 0, 0, 0, 0, 0, 0, 0], 64),
    ([0x2001, 2, 0, 0, 0, 0, 0, 0], 48),
    ([0x2001, 0x10, 0, 0, 0, 0, 0, 0], 28),
    ([0x2001, 0x20, 0, 0, 0, 0, 0, 0], 28),
];

fn in_prefix(address: u128, network: u128, prefix: u32, bits: u32) -> bool {
    let shift = bits - prefix;
    prefix == 0 || (address >> shift) == (network >> shift)
}

fn public_v4(octets: [u8; 4]) -> bool {
    let value = u128::from(u32::from_be_bytes(octets));
    !V4_BLOCKED
        .iter()
        .any(|(net, prefix)| in_prefix(value, u128::from(u32::from_be_bytes(*net)), *prefix, 32))
}

fn segments_value(segments: [u16; 8]) -> u128 {
    segments
        .iter()
        .fold(0u128, |value, segment| (value << 16) | u128::from(*segment))
}

fn public_v6(segments: [u16; 8]) -> bool {
    let value = segments_value(segments);
    !V6_BLOCKED
        .iter()
        .any(|(net, prefix)| in_prefix(value, segments_value(*net), *prefix, 128))
}

/// IPv4-mapped (`::ffff:a.b.c.d`) and DNS64 (`64:ff9b::/96`) carry an IPv4
/// target that the IPv4 policy decides.
fn carried_v4(segments: [u16; 8]) -> Option<[u8; 4]> {
    let mapped = segments[..6] == [0, 0, 0, 0, 0, 0xffff];
    let dns64 = segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0];
    let [high, low] = [segments[6].to_be_bytes(), segments[7].to_be_bytes()];
    (mapped || dns64).then_some([high[0], high[1], low[0], low[1]])
}

/// Node `assertWebFetchLiteralEgress`.
pub fn literal_guard(url: &Url) -> Result<(), WebError> {
    let host = literal_host(url.host_str().unwrap_or("").trim());
    if host == "localhost" || host.ends_with(".localhost") {
        return Err(WebError::new(
            "webfetch_egress_blocked",
            "WebFetch cannot access private or local hostnames",
        ));
    }
    let public = match url.host() {
        Some(url::Host::Ipv4(address)) => public_v4(address.octets()),
        Some(url::Host::Ipv6(address)) => match carried_v4(address.segments()) {
            Some(octets) => public_v4(octets),
            None => public_v6(address.segments()),
        },
        _ => true,
    };
    if public {
        return Ok(());
    }
    Err(WebError::new(
        "webfetch_egress_blocked",
        "WebFetch cannot access private or local IP addresses",
    ))
}
