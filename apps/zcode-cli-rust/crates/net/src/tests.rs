//! Parity with the TS egress implementation. Fixtures come from
//! `scripts/zcode-cli-rust-egress-fixtures.mjs`, which runs the Node functions.
use super::*;
use serde_json::{Map, Value, json};

fn fixtures() -> Value {
    serde_json::from_str(include_str!("../fixtures/egress.json")).unwrap()
}

fn pairs(value: &Value) -> Vec<(String, String)> {
    value
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
        .collect()
}

fn object(pairs: impl IntoIterator<Item = (String, String)>) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k, v.into())).collect())
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn resolution(resolved: &ProxyResolution) -> Value {
    let mut value = json!({"noProxyMatched": resolved.no_proxy_matched});
    if let Some(source) = &resolved.source {
        value["proxySource"] = source.as_str().into();
    }
    if let Some(proxy) = &resolved.proxy {
        value["proxyUrl"] = proxy.as_str().into();
    }
    value
}

#[test]
fn proxy_resolution_matches_node() {
    for case in fixtures()["proxy"].as_array().unwrap() {
        let options = &case["options"];
        let policy = NetworkPolicy {
            http_proxy: text(options, "httpProxy"),
            no_proxy: text(options, "noProxy"),
            ca_cert_file: None,
        };
        let env = options.get("env").map(pairs).unwrap_or_default();
        let lookup = |key: &str| env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
        let rules = ProxyRules::new(&policy, lookup);
        let url = case["url"].as_str().unwrap();
        assert_eq!(
            resolution(&rules.resolve(url, false)),
            case["request"],
            "{url}"
        );
        assert_eq!(
            resolution(&rules.resolve(url, true)),
            case["webFetch"],
            "{url}"
        );
    }
}

#[test]
fn tool_child_env_matches_node() {
    for case in fixtures()["childEnv"].as_array().unwrap() {
        let network = &case["network"];
        let policy = NetworkPolicy {
            http_proxy: text(network, "httpProxy"),
            no_proxy: text(network, "noProxy"),
            ca_cert_file: text(network, "caCertFile"),
        };
        let windows = case["platform"] == "win32";
        let env = tool_env(&pairs(&case["sourceEnv"]), &policy, windows);
        assert_eq!(object(env), case["expected"], "{}", case["sourceEnv"]);
    }
}

#[test]
fn runtime_env_capture_matches_node() {
    for case in fixtures()["runtimeEnv"].as_array().unwrap() {
        let argv = ["/usr/bin/zcode".to_owned(), "app-server".to_owned()];
        let captured = RuntimeEnv::capture(pairs(&case["env"]), Path::new("/home/u"), &argv);
        assert_eq!(
            object(captured.vars().to_vec()),
            case["expected"],
            "{}",
            case["env"]
        );
    }
}

#[test]
fn beta_entry_points_get_a_separate_storage_dir() {
    let home = Path::new("/home/u");
    let beta = |env: Vec<(&str, &str)>, argv: &[&str]| {
        let env = env.into_iter().map(|(k, v)| (k.into(), v.into()));
        let argv: Vec<String> = argv.iter().map(|a| (*a).to_owned()).collect();
        RuntimeEnv::capture(env, home, &argv)
            .get("ZCODE_STORAGE_DIR")
            .map(str::to_owned)
    };
    let expected = Some(home.join(".zcode-beta").to_string_lossy().into_owned());
    assert_eq!(beta(vec![("ZCODE_BETA", "1")], &[]), expected);
    assert_eq!(beta(vec![], &["/opt/bin/zcode-beta"]), expected);
    assert_eq!(beta(vec![], &["C:\\zcode-beta.exe"]), expected);
    assert_eq!(beta(vec![], &["/opt/zcode-beta-x"]), None);
    assert_eq!(
        beta(
            vec![("ZCODE_ENV", "beta"), ("ZCODE_STORAGE_DIR", "/s")],
            &[]
        )
        .as_deref(),
        Some("/s")
    );
}

#[test]
fn gateway_rewrite_matches_node() {
    for case in fixtures()["gateway"].as_array().unwrap() {
        let env = pairs(&case["env"]);
        let origin = headers::endpoint_origin(|key| {
            env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
        })
        .unwrap();
        let url = case["url"].as_str().unwrap();
        let rewritten = gateway::rewrite(url, &origin);
        assert_eq!(
            json!({"viaGateway": rewritten.is_some(), "url": rewritten.as_deref().unwrap_or(url)}),
            case["expected"],
            "{url}"
        );
    }
}

#[test]
fn header_merging_matches_node() {
    for case in fixtures()["mergeHeaders"].as_array().unwrap() {
        let mut merged = Headers::default();
        for source in case["sources"].as_array().unwrap() {
            if let Some(source) = source.as_object() {
                merged.extend(
                    source
                        .iter()
                        .map(|(k, v)| (k.as_str(), v.as_str().unwrap())),
                );
            }
        }
        let merged: Map<String, Value> = merged
            .iter()
            .map(|(k, v)| (k.to_owned(), v.into()))
            .collect();
        assert_eq!(Value::Object(merged), case["expected"]);
    }
}

#[test]
fn attribution_headers_match_node() {
    for case in fixtures()["attribution"].as_array().unwrap() {
        let context = &case["context"];
        let headers = headers::attribution(&headers::Attribution {
            request_id: context["requestId"].as_str().unwrap(),
            session_type: context["modelRequestSessionType"].as_str().unwrap(),
            trace_id: context["traceId"].as_str().unwrap(),
            query_id: context["queryId"].as_str(),
            session_id: context["sessionId"].as_str(),
            base_url: context["baseURL"].as_str().unwrap_or(""),
        });
        let headers: Vec<Value> = headers.iter().map(|(k, v)| json!([k, v])).collect();
        let expected: Vec<Value> = case["expected"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| json!([k, v]))
            .collect();
        let mut sorted = headers.clone();
        sorted.sort_by_key(|h| h[0].as_str().unwrap().to_owned());
        assert_eq!(sorted, expected, "{context}");
    }
}

#[test]
fn openrouter_and_identity_match_node() {
    let fixtures = fixtures();
    for case in fixtures["openRouter"].as_array().unwrap() {
        let mut headers = Headers::default();
        headers.set("X-Title", "Z Code@electron");
        headers::with_openrouter(&mut headers, case["baseUrl"].as_str().unwrap_or(""));
        let headers: Map<String, Value> = headers
            .iter()
            .map(|(k, v)| (k.to_owned(), v.into()))
            .collect();
        assert_eq!(
            Value::Object(headers),
            case["expected"],
            "{}",
            case["baseUrl"]
        );
    }
    for case in fixtures["identity"].as_array().unwrap() {
        let env = pairs(&case["env"]);
        let identity = headers::identity(
            |key| env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()),
            "electron",
            "1.2.3",
        )
        .unwrap();
        let expected = case["expected"].as_object().unwrap();
        for (key, value) in identity.iter() {
            if let Some(expected) = expected.get(key) {
                assert_eq!(value, expected.as_str().unwrap(), "{key}");
            }
        }
        for key in expected.keys() {
            assert!(identity.contains(key), "{key}");
        }
        assert!(identity.contains("X-Os-Category"));
    }
}

#[test]
fn invalid_endpoint_origin_is_rejected() {
    let env = [("ZCODE_BASE_URL", "ftp://x")];
    let lookup = |key: &str| env.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
    assert!(headers::endpoint_origin(lookup).is_err());
}

#[test]
fn locale_and_timezone_follow_icu() {
    let language = |pairs: &[(&str, &str)]| {
        platform::language(|key| pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| *v))
    };
    assert_eq!(language(&[("LANG", "zh_CN.UTF-8")]), "zh-CN");
    assert_eq!(
        language(&[("LC_ALL", "fr_FR.UTF-8"), ("LANG", "zh_CN")]),
        "fr-FR"
    );
    assert_eq!(
        language(&[("LC_MESSAGES", "ko_KR"), ("LANG", "zh_CN")]),
        "ko-KR"
    );
    assert_eq!(language(&[("LANG", "C.UTF-8")]), "en-US");
    assert_eq!(language(&[("LANG", "")]), "und");
    assert_eq!(language(&[("LANG", "sr_RS@latin")]), "sr-RS-latin");
    assert_eq!(language(&[]), "en-US");
    let timezone = |tz: &str| platform::timezone(|key| (key == "TZ").then_some(tz));
    assert_eq!(timezone("").as_deref(), Some("Etc/Unknown"));
    assert_eq!(timezone("Bogus/Zone"), None);
    if cfg!(unix) {
        assert_eq!(timezone(":Europe/Paris").as_deref(), Some("Europe/Paris"));
    }
}

use std::path::Path;

#[tokio::test]
async fn device_id_is_persisted_once_and_shared() {
    let root = tempfile::tempdir().unwrap();
    let file = device::state_file(None, root.path(), root.path());
    tokio::fs::create_dir_all(file.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&file, r#"{"other":1}"#).await.unwrap();
    let first = device::ensure(&file).await;
    let state: Value = serde_json::from_slice(&tokio::fs::read(&file).await.unwrap()).unwrap();
    assert_eq!(state["deviceMid"], first.as_str());
    assert_eq!(state["other"], 1);
    assert_eq!(device::ensure(&file).await, first);
    assert!(!file.with_file_name("telemetry-state.lock").exists());
}

#[tokio::test]
async fn stale_device_lock_from_a_dead_process_is_taken_over() {
    let root = tempfile::tempdir().unwrap();
    let file = device::state_file(Some("~/data"), root.path(), Path::new("/"));
    assert!(file.starts_with(root.path().join("data/.zcode/v2")));
    tokio::fs::create_dir_all(file.parent().unwrap())
        .await
        .unwrap();
    let lock = file.with_file_name("telemetry-state.lock");
    // 取一个已退出进程的 pid：锁未超时但持有者已死，应被接管。
    let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" });
    if cfg!(windows) {
        child.args(["/C", "exit"]);
    }
    let mut child = child.spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    tokio::fs::write(&lock, json!({"createdAt": 1, "pid": pid}).to_string())
        .await
        .unwrap();
    let id = device::ensure(&file).await;
    let state: Value = serde_json::from_slice(&tokio::fs::read(&file).await.unwrap()).unwrap();
    assert_eq!(state["deviceMid"], id.as_str());
}
