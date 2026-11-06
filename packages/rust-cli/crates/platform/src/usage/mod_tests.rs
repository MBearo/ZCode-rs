//! 计划清单的原料与共用的小工具（platform.md §11.2、§11.3）。

use serde_json::json;

use super::*;
use crate::fake_server_tests::{Harness, ZAI};

const IDENTITY: &str = "key-0123456789abcdef01234567";

/// 一份手工 key 的登录（个人 Coding Plan）。
pub(super) fn manual_login(provider: &str, key: &str) -> Value {
    json!({
        account::identity_key(provider): IDENTITY,
        account::source_key(provider): "manual",
        account::manual_key(provider, IDENTITY): key,
    })
}

/// 内置配置的六个账号 provider 都在清单里（start plan 不在 family 的个人 / 团队里）；快照里有模板的个人 provider
/// 已授权，并带显示名与密钥管理页。
#[tokio::test]
async fn the_view_lists_the_builtin_account_providers() {
    let harness = Harness::new("http://127.0.0.1:9").await;
    let empty = harness.platform.usage_view().await;
    assert_eq!(empty.families.len(), 2);
    assert_eq!(empty.families[0].family, Family::Zai);
    assert_eq!(empty.families[1].family, Family::Bigmodel);
    assert_eq!(
        empty.families[0].individual,
        Some(IndividualProvider {
            provider: ZAI.to_owned(),
            entitled: false
        })
    );
    assert_eq!(
        empty.families[1].team,
        Some(TeamProvider {
            provider: "account:bigmodel-team-coding-plan".to_owned(),
            teams: Vec::new()
        })
    );
    harness
        .write_credentials(&manual_login(ZAI, "sk-zai"))
        .await;
    let view = harness.platform.usage_view().await;
    assert!(view.families[0].individual.as_ref().unwrap().entitled);
    assert!(!view.families[1].individual.as_ref().unwrap().entitled);
    // 账号 provider 没有模板，也就没有密钥管理页（App 相同）。
    let zai = view.providers.iter().find(|p| p.provider == ZAI).unwrap();
    assert_eq!(zai.name, "Z.AI Individual Coding Plan");
    assert_eq!(zai.key_url, None);
}

/// 从模板建的个人 provider：规则的 `access` 整个盖住模板的，密钥管理页取模板的 `access.apiKeyManagementUrl`（usageHost
/// `keyUrlOf(template.config.access)`）。
#[tokio::test]
async fn personal_providers_carry_the_key_page_of_their_template() {
    let harness = Harness::new("http://127.0.0.1:9").await;
    let personal = json!({"schemaVersion": 1, "config": {"providerConfigRules": {"providerRules": [{
        "providerId": "personal:mine", "providerName": "Mine", "enabled": true, "templateId": "zai-api",
        "config": {"group": "zai-family", "access": {"type": "zhipu-coding-plan-api-key", "apiKey": "sk-mine"}}
    }]}}});
    crate::test_support::write(
        harness.paths.provider_config().as_path(),
        personal.to_string(),
    )
    .await;
    let view = harness.platform.usage_view().await;
    let mine = view
        .providers
        .iter()
        .find(|p| p.provider == "personal:mine");
    assert_eq!(
        mine.and_then(|p| p.key_url.as_deref()),
        Some("https://z.ai/manage-apikey/apikey-list"),
        "{view:?}"
    );
}

/// JS `URLSearchParams.set` 与 `pathname.replace(/…$/, …)`。
#[test]
fn urls_are_rewritten_like_the_app() {
    let mut url =
        Url::parse("https://example.com/api/monitor/usage/quota/limit?type=9&a=1&type=8").unwrap();
    set_param(&mut url, "type", "2");
    set_param(&mut url, "startTime", "2026-10-30 00:00:00");
    assert_eq!(
        url.as_str(),
        "https://example.com/api/monitor/usage/quota/limit?type=2&a=1&startTime=2026-10-30+00%3A00%3A00"
    );
    replace_path_end(&mut url, "/usage/quota/limit", "/credit-usage/activity");
    assert_eq!(url.path(), "/api/monitor/credit-usage/activity");
    // 不以它结尾时不变。
    replace_path_end(&mut url, "/quota/limit", "/model-performance-day");
    assert_eq!(url.path(), "/api/monitor/credit-usage/activity");
}

#[test]
fn envelopes_and_numbers_follow_the_app() {
    assert!(successful(&json!({})));
    assert!(successful(&json!({"code": null})));
    assert!(successful(&json!({"code": 200, "success": true})));
    assert!(!successful(&json!({"code": 200, "success": false})));
    assert!(!successful(&json!({"code": "200"})));
    assert!(!successful(&json!({"code": 1001, "msg": "token expired"})));
    assert!(explicitly_successful(&json!({"code": 0})));
    assert!(explicitly_successful(&json!({"success": true})));
    assert!(!explicitly_successful(&json!({})));
    assert_eq!(
        envelope_message(&json!({"msg": " ", "message": " bad "})),
        "bad"
    );
    assert_eq!(finite(&json!(" 12.5 ")), Some(12.5));
    assert_eq!(finite(&json!("")), None);
    assert_eq!(finite(&json!("x")), None);
    assert_eq!(number(&json!("12")), None);
    assert_eq!(text(&json!("  ")), None);
}
