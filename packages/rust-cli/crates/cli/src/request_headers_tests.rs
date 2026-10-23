//! 来源头的事实（工作线 F64，model.md §2.4）：与 Node 的对照夹具逐项相同（`scripts/rust-cli/headers-golden.mjs` 用 Node 的
//! `buildCliZCodeSourceHeaders` 与 `Intl.DateTimeFormat` 生成 `tests/source-headers-golden.json`），以及 Windows 以外的两项读法。

use serde_json::Value;

use super::*;

fn golden() -> Value {
    serde_json::from_str(include_str!("../tests/source-headers-golden.json")).unwrap()
}

fn env_of(case: &Value) -> impl Fn(&str) -> Option<String> + '_ {
    move |name: &str| case["env"][name].as_str().map(str::to_owned)
}

fn text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

/// 来源头：版本的来源（`ZCODE_APP_VERSION` 优先，空值也算）、服务地址、发布通道、不可打印的值与平台，结果（名字转小写、
/// 键序）与 Node 相同。
#[test]
fn source_headers_match_node() {
    let golden = golden();
    let cases = golden["source"].as_array().unwrap();
    assert!(cases.len() >= 10);
    for case in cases {
        let var = env_of(case);
        let host = Host {
            os_release: text(&case["release"]),
            localtime: None,
        };
        let mut facts = facts(&var, case["appVersion"].as_str().unwrap(), &host).unwrap();
        facts.locale = text(&case["locale"]);
        facts.time_zone = text(&case["timeZone"]);
        facts.platform = text(&case["platform"]).unwrap();
        facts.arch = text(&case["arch"]).unwrap();
        let ours = rust_cli_model::source_headers(&facts);
        let node: Vec<(String, String)> = case["headers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| {
                (
                    pair[0].as_str().unwrap().to_lowercase(),
                    pair[1].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        assert_eq!(ours, node, "{case}");
    }
}

/// locale：Unix 上与 Node 的 `Intl.DateTimeFormat` 相同（每个用例是只带这几个变量的 node 进程读到的值）。
#[test]
fn the_client_language_matches_node() {
    let golden = golden();
    let cases = golden["locales"].as_array().unwrap();
    assert!(cases.len() >= 60);
    for case in cases {
        assert_eq!(
            client_language(&env_of(case)),
            case["locale"].as_str().unwrap(),
            "{case}"
        );
    }
}

/// 设了 `TZ` 时的时区名与 Node 的 `Intl.DateTimeFormat` 相同；Node 给不出时（`undefined`）本工程也是 None（头里写 `unknown`）。
#[test]
fn the_client_time_zone_matches_node() {
    let golden = golden();
    let cases = golden["timeZones"].as_array().unwrap();
    assert!(cases.len() >= 40);
    for case in cases {
        assert_eq!(
            client_time_zone(
                case["tz"].as_str(),
                Some("/var/db/timezone/zoneinfo/Asia/Tokyo")
            ),
            text(&case["timeZone"]),
            "{case}"
        );
    }
}

/// 没设 `TZ` 时取 `/etc/localtime` 链接目标里最后一个 `zoneinfo/` 之后的部分，再换成 CLDR 的旧名；不是链接时没有。
#[test]
fn without_tz_the_localtime_link_names_the_zone() {
    for (link, expected) in [
        (
            "/var/db/timezone/zoneinfo/Asia/Shanghai",
            Some("Asia/Shanghai"),
        ),
        ("../usr/share/zoneinfo/Europe/Berlin", Some("Europe/Berlin")),
        ("/usr/share/zoneinfo/Asia/Kolkata", Some("Asia/Calcutta")),
        ("/etc/zones/UTC", None),
    ] {
        assert_eq!(
            client_time_zone(None, Some(link)).as_deref(),
            expected,
            "{link}"
        );
    }
    assert_eq!(client_time_zone(None, None), None);
}

/// 版本默认是本工程的；ZCode 服务地址无效时报错（启动失败，Node 建 App 时同样抛错）。
#[test]
fn the_version_and_the_service_address() {
    let none = |_: &str| None;
    let host = Host::default();
    let facts = facts(&none, "0.1.52", &host).unwrap();
    assert_eq!(facts.version, "0.1.52");
    assert_eq!(facts.endpoint_origin, "https://zcode.z.ai");
    assert_eq!(facts.release_channel, "production");
    assert_eq!(facts.platform, node_platform());
    assert_eq!(facts.arch, node_arch());
    let bad = |name: &str| (name == "ZCODE_BASE_URL").then(|| "ftp://x".to_owned());
    assert!(super::facts(&bad, "0.1.52", &host).is_err());
}

/// 本机：系统版本非空；trace ID 每个进程一个，`x-request-id` 每次取新的。
#[test]
fn the_host_and_the_ids() {
    assert!(host().os_release.is_some_and(|release| !release.is_empty()));
    let clock: Arc<dyn rust_cli_kernel_api::clock::Clock> =
        Arc::new(rust_cli_node_store::SystemClock::utc());
    let ids = Arc::new(NodeIds::new(clock));
    let none = |_: &str| None;
    let headers = request_headers(&none, "0.1.52", ids).unwrap();
    let (first, second) = ((headers.request_id)(), (headers.request_id)());
    assert_ne!(first, second);
    assert_ne!(headers.trace_id, first);
    assert_eq!(headers.trace_id.len(), 36);
}
