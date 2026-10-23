//! 官方账号模型的 overlay 与请求期凭据（platform.md §5），键名与 Node 独立 CLI 相同。

use rust_cli_paths::StoragePaths;
use serde_json::json;

use super::*;
use crate::credentials::{Cipher, CredentialSecret, CredentialStore};
use crate::providers::BUNDLED_BUILTIN;
use crate::test_support::write;

const ZAI: &str = "account:zai-individual-coding-plan";
const BIGMODEL: &str = "account:bigmodel-individual-coding-plan";

fn secret() -> CredentialSecret {
    CredentialSecret {
        explicit: Some(RedactedString::new("test-secret")),
        platform: "linux".to_owned(),
        home: "/home/dev".to_owned(),
        username: "dev".to_owned(),
    }
}

/// 把明文记录加密写进临时目录的 credentials.json，再读回来。
async fn credentials(record: &[(&str, &str)]) -> (tempfile::TempDir, Credentials) {
    let dir = tempfile::tempdir().unwrap();
    let paths = StoragePaths::resolve(&Default::default(), dir.path(), dir.path());
    let cipher = Cipher::new(&secret());
    let encrypted: serde_json::Map<String, Value> = record
        .iter()
        .enumerate()
        .map(|(index, (key, value))| {
            let iv = [u8::try_from(index).unwrap(); 12];
            (
                (*key).to_owned(),
                json!(cipher.encrypt_with_iv(value, iv).unwrap()),
            )
        })
        .collect();
    write(
        paths.credentials().as_path(),
        Value::Object(encrypted).to_string(),
    )
    .await;
    let store = CredentialStore::new(paths.credentials(), &secret());
    let read = store.read().await.unwrap();
    (dir, read)
}

fn builtin() -> Value {
    serde_json::from_str::<Value>(BUNDLED_BUILTIN).unwrap()["config"].clone()
}

/// 没有可用团队项目时的 overlay；`subscribed` 是个人订阅查询给出 entitled 的 provider（`personal/`，§5.7）。
fn overlay(
    builtin: &Value,
    credentials: Option<&Credentials>,
    subscribed: &[&str],
) -> AccountLayer {
    let personal: HashMap<String, bool> = subscribed
        .iter()
        .map(|provider| ((*provider).to_owned(), true))
        .collect();
    let view = AccountView {
        credentials,
        personal: &personal,
        teams: &HashMap::new(),
    };
    super::overlay(builtin, &view)
}

fn entitled(layer: &AccountLayer, provider: &str) -> bool {
    layer.providers[provider]["access"]["entitled"] == true
}

#[test]
fn keys_match_node() {
    assert_eq!(
        identity_key(" account:x "),
        "account-provider:account:x:identity"
    );
    assert_eq!(
        source_key("account:x"),
        "account-provider:account:x:auth-source"
    );
    assert_eq!(
        manual_key("account:x", "key-ab12"),
        "account-provider:coding-plan:account:x:account:key-ab12:api-key"
    );
    // encodeURIComponent 的保留字符与 UTF-8 编码。
    assert_eq!(
        encode_uri_component("a b/é!*'()~._-"),
        "a%20b%2F%C3%A9!*'()~._-"
    );
}

#[tokio::test]
async fn individual_plans_follow_the_manual_key_or_the_app_login() {
    let identity = "key-0123456789abcdef01234567";
    let (_dir, manual) = credentials(&[
        (&identity_key(ZAI), identity),
        (&source_key(ZAI), "manual"),
        (&manual_key(ZAI, identity), "sk-zai-manual"),
        (&identity_key(BIGMODEL), "u1"),
    ])
    .await;
    // 手工 key：有 key 就授权，不看订阅；订阅结论不能盖过手工 key 的来源。
    let layer = overlay(&builtin(), Some(&manual), &[]);
    assert!(entitled(&layer, ZAI));
    // Node 独立 CLI 的身份（没有 App 的 profile id）：不算登录，只看订阅结论（工作线 F50）。
    assert!(!entitled(&layer, BIGMODEL));
    assert!(entitled(
        &overlay(&builtin(), Some(&manual), &[BIGMODEL]),
        BIGMODEL
    ));
    // 独立 CLI 不支持的账号 provider 显式不授权。
    assert!(!entitled(&layer, "account:zai-start-plan"));
    assert!(
        layer
            .providers
            .values()
            .all(|p| p["access"]["type"] == "zhipu-account")
    );
    // 不写 current，团队 provider 没有可用的团队项目时不授权。
    assert!(layer.states.is_empty());
    assert!(!entitled(&layer, "account:zai-team-coding-plan"));
    // 凭据读不到：全部不授权（订阅结论也不算）。
    assert!(
        !overlay(&builtin(), None, &[ZAI, BIGMODEL])
            .providers
            .values()
            .any(|p| p["access"]["entitled"] == true)
    );
    // 手工 key 缺 key：不授权。
    let (_dir, broken) =
        credentials(&[(&identity_key(ZAI), identity), (&source_key(ZAI), "manual")]).await;
    assert!(!entitled(&overlay(&builtin(), Some(&broken), &[ZAI]), ZAI));
}

#[tokio::test]
async fn personal_accounts_read_the_app_profile_id() {
    let identity = "key-0123456789abcdef01234567";
    let (_dir, both) = credentials(&[
        (&identity_key(ZAI), identity),
        (&source_key(ZAI), "manual"),
        (&manual_key(ZAI, identity), "sk"),
        ("oauth:zai:user_info", r#"{"user_id":"u-zai"}"#),
        ("oauth:zai:access_token", "jwt"),
        (
            "oauth:bigmodel:user_info",
            r#"{"id":"u-big","username":"a","displayName":"A"}"#,
        ),
        ("oauth:bigmodel:access_token", "at"),
        (&identity_key(BIGMODEL), "node-identity"),
        (&source_key(BIGMODEL), "oauth"),
    ])
    .await;
    let accounts = personal_accounts(&builtin(), Some(&both));
    // 来源是 manual 的不查订阅；OAuth 的身份是 App 的 profile id，不是 Node 的 identity。
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].provider, BIGMODEL);
    assert_eq!(accounts[0].identity.as_deref(), Some("u-big"));
    assert_eq!(
        accounts[0].token.as_ref().map(RedactedString::expose),
        Some("at")
    );
    assert!(personal_accounts(&builtin(), None).is_empty());
}

#[tokio::test]
async fn request_credentials_follow_the_login_source() {
    let identity = "key-0123456789abcdef01234567";
    let (_dir, manual) = credentials(&[
        (&identity_key(ZAI), identity),
        (&source_key(ZAI), "manual"),
        (&manual_key(ZAI, identity), " sk-zai-manual "),
        ("oauth:zai:user_info", r#"{"user_id":"u-zai"}"#),
        ("oauth:zai:access_token", "jwt-zai"),
        (
            "oauth:bigmodel:user_info",
            r#"{"id":"u-big","username":"a","displayName":"A"}"#,
        ),
        ("oauth:bigmodel:access_token", "at"),
        ("zcodejwttoken", "zcode-jwt"),
    ])
    .await;
    // 手工 key 盖过 App 的 OAuth 登录。
    let Ok(RequestAuth::Manual(key)) = request_auth(&manual, ZAI, "zai", "individual-coding-plan")
    else {
        panic!("manual login uses the stored key");
    };
    assert_eq!(key.expose(), "sk-zai-manual");
    // 没有来源、只有 App 写的键：照 App 按 profile id 换取，请求期不补写来源。
    let Ok(RequestAuth::OAuth(login)) =
        request_auth(&manual, BIGMODEL, "bigmodel", "individual-coding-plan")
    else {
        panic!("the App login is an OAuth login");
    };
    assert_eq!(
        (
            login.identity.as_str(),
            login.access_token.expose(),
            login.zcode_jwt.as_ref().map(RedactedString::expose)
        ),
        ("u-big", "at", Some("zcode-jwt"))
    );
    let other = request_auth(&manual, "account:zai-start-plan", "zai", "start-plan");
    assert!(matches!(other, Err(ModelError::Authentication { .. })));
    let (_dir, empty) = credentials(&[]).await;
    assert!(matches!(
        request_auth(&empty, ZAI, "zai", "individual-coding-plan"),
        Err(ModelError::Authentication { .. })
    ));
    // Node 独立 CLI 的身份与来源、有 token，没有 user_info：没有 profile id，要求重新登录（F50 以前算 OAuth 登录）。
    let (_dir, node) = credentials(&[
        (&identity_key(ZAI), "u2"),
        (&source_key(ZAI), "oauth"),
        ("oauth:zai:access_token", "jwt"),
    ])
    .await;
    assert!(matches!(
        request_auth(&node, ZAI, "zai", "individual-coding-plan"),
        Err(ModelError::Authentication { ref message }) if message.starts_with("project_token_login_required")
    ));
}

// profile id（platform.md §9.8，App `OAuthCredentialRepo.loadUserProfile`；F47 的 connection_tests.rs 搬来）。

/// 明文写进临时目录的 credentials.json（Node 也接受没有 `enc:v1:` 前缀的旧值），再读回来。
async fn plain_credentials(record: &[(&str, &str)]) -> (tempfile::TempDir, Credentials) {
    let dir = tempfile::tempdir().unwrap();
    let paths = rust_cli_paths::StoragePaths::resolve(&Default::default(), dir.path(), dir.path());
    let object: Map<String, Value> = record
        .iter()
        .map(|(key, value)| ((*key).to_owned(), json!(value)))
        .collect();
    crate::test_support::write(
        paths.credentials().as_path(),
        Value::Object(object).to_string(),
    )
    .await;
    let store = crate::credentials::CredentialStore::new(
        paths.credentials(),
        &crate::fake_server_tests::secret(),
    );
    let credentials = store.read().await.unwrap();
    (dir, credentials)
}

#[tokio::test]
async fn profile_ids_follow_the_app() {
    let cases: [(&str, &str, Option<&str>); 7] = [
        // App 的 profile：三个字段都是字符串时取 id（原值，不去空白）。
        (
            "bigmodel",
            r#"{"id":" key-1 ","username":"dev","displayName":"Dev"}"#,
            Some(" key-1 "),
        ),
        ("bigmodel", r#"{"id":"key-1","username":"dev"}"#, None),
        // zai 的后端原始用户对象（本工程与 Node 登录时这样存）：取 user_id。
        ("zai", r#"{"user_id":"u-1","name":"Dev"}"#, Some("u-1")),
        ("zai", r#"{"name":"Dev"}"#, Some("unknown")),
        ("zai", r#"{"avatar":"x"}"#, None),
        (
            "zai",
            r#"{"id":"   ","username":"a","displayName":"b"}"#,
            None,
        ),
        ("zai", "not json", None),
    ];
    for (family, info, expected) in cases {
        let key = format!("oauth:{family}:user_info");
        let (_dir, credentials) = plain_credentials(&[(key.as_str(), info)]).await;
        let family = Family::parse(family).unwrap();
        assert_eq!(
            profile_id(&credentials, family).as_deref(),
            expected,
            "{info}"
        );
    }
}

/// profile id 的读法与 App 对拍（`app-login-golden.json` 的 `profiles`：App `loadUserProfile` 对各种 user_info 读出的
/// id）。App 读出去空白后为空的 id 时，它的两条路径都当作没有身份（`trim()` 后为空），本工程直接给 None。
#[tokio::test]
async fn profile_ids_match_the_app_golden() {
    let golden: Value =
        serde_json::from_str(include_str!("../tests/fixtures/app-login-golden.json")).unwrap();
    let cases = golden["profiles"].as_object().unwrap();
    assert!(cases.len() >= 10);
    for (name, case) in cases {
        for family in ["zai", "bigmodel"] {
            let key = format!("oauth:{family}:user_info");
            let value = case["value"].as_str().unwrap();
            let (_dir, credentials) = plain_credentials(&[(key.as_str(), value)]).await;
            let expected = case[family]
                .as_str()
                .filter(|id| !id.trim().is_empty())
                .map(str::to_owned);
            assert_eq!(
                profile_id(&credentials, Family::parse(family).unwrap()),
                expected,
                "{name} ({family})"
            );
        }
    }
}
