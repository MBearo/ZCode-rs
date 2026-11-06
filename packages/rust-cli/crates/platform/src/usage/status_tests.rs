//! 内置家族的账号状态（platform.md §11.4）。
use serde_json::json;

use super::*;
use crate::fake_server_tests::{Harness, ZAI};
use crate::usage::tests::manual_login;

/// 没有凭据时两个家族都没有登录、算个人的计划；OAuth 登录（profile 与 access token）算登录，账号取 user_info 的邮箱；
/// 个人 provider 的手工 key 也算登录。
#[tokio::test]
async fn sign_in_comes_from_the_oauth_profile_or_a_manual_key() {
    let harness = Harness::new("http://127.0.0.1:9").await;
    let none = harness.platform.family_accounts().await;
    assert_eq!(
        none.iter().map(|f| f.family).collect::<Vec<_>>(),
        [Family::Zai, Family::Bigmodel]
    );
    assert!(none.iter().all(|f| !f.signed_in && f.account.is_empty()));
    assert_eq!(none[0].plan, AccountPlan::Individual);
    assert_eq!(none[0].provider, ZAI);
    harness
        .write_credentials(&json!({
            "oauth:zai:access_token": "at-zai",
            "oauth:zai:user_info": json!({"user_id": "u1", "name": "Dev", "email": "dev@example.com"}).to_string(),
        }))
        .await;
    let oauth = harness.platform.family_accounts().await;
    assert!(oauth[0].signed_in);
    assert_eq!(oauth[0].account, "dev@example.com");
    assert!(!oauth[1].signed_in);
    harness
        .write_credentials(&manual_login(ZAI, "sk-zai"))
        .await;
    let manual = harness.platform.family_accounts().await;
    assert!(manual[0].signed_in);
    assert_eq!(manual[0].account, "");
}

/// bigmodel 的 user_info 是 App 的 profile：账号取它的原始对象里的邮箱，没有时是显示名。
#[tokio::test]
async fn the_bigmodel_profile_names_the_account() {
    let harness = Harness::new("http://127.0.0.1:9").await;
    let profile = |raw: serde_json::Value| {
        json!({"id": "1", "username": "dev", "displayName": "Dev", "rawProfile": raw}).to_string()
    };
    harness
        .write_credentials(&json!({
            "oauth:bigmodel:access_token": "at-bm",
            "oauth:bigmodel:user_info": profile(json!({"email": "dev@example.com"})),
        }))
        .await;
    let with_email = harness.platform.family_accounts().await;
    assert!(with_email[1].signed_in);
    assert_eq!(with_email[1].account, "dev@example.com");
    harness
        .write_credentials(&json!({
            "oauth:bigmodel:access_token": "at-bm",
            "oauth:bigmodel:user_info": profile(json!({})),
        }))
        .await;
    assert_eq!(harness.platform.family_accounts().await[1].account, "Dev");
}
