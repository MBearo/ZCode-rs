//! 项目令牌的键与归一（platform.md §5.4）：与 Node 的写法逐字相同。

use super::*;

#[test]
fn login_tokens_drop_a_bearer_prefix() {
    assert_eq!(login_token("  Bearer   abc "), "abc");
    assert_eq!(login_token("bearer\tabc"), "abc");
    assert_eq!(login_token("Bearerabc"), "Bearerabc");
    assert_eq!(login_token(" raw "), "raw");
}

#[test]
fn fingerprints_are_sha256_hex() {
    assert_eq!(
        fingerprint("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn location_keys_match_the_app() {
    // App：encodeURIComponent(JSON.stringify(["https://api.z.ai","zai","user 1/é",null,1,"non-team"]))。修复线 F47
    // 以前按 Node 独立 CLI 写 "default"（不排除团队项目）。
    assert_eq!(
        location::storage_key("https://api.z.ai", Family::Zai, "user 1/é", None),
        "project-key-location:v1:%5B%22https%3A%2F%2Fapi.z.ai%22%2C%22zai%22%2C%22user%201%2F%C3%A9%22%2Cnull%2C1%2C%22non-team%22%5D"
    );
    // 团队：[…, {organizationId, projectId}, 1, null]。
    let team = TeamProject {
        organization_id: "org t".to_owned(),
        project_id: "p/t".to_owned(),
    };
    assert_eq!(
        location::storage_key("https://bigmodel.cn", Family::Bigmodel, "u-1", Some(&team)),
        "project-key-location:v1:%5B%22https%3A%2F%2Fbigmodel.cn%22%2C%22bigmodel%22%2C%22u-1%22%2C%7B%22organizationId%22%3A%22org%20t%22%2C%22projectId%22%3A%22p%2Ft%22%7D%2C1%2Cnull%5D"
    );
    assert_eq!(
        location::scope_key("u-1", Some(&team)),
        r#"["u-1",{"organizationId":"org t","projectId":"p/t"},null]"#
    );
    assert_eq!(
        location::scope_key("u-1", None),
        r#"["u-1",null,"non-team"]"#
    );
    let location = Location {
        organization_id: "org/1".to_owned(),
        project_id: "p 1".to_owned(),
        api_key_id: "k1".to_owned(),
    };
    assert_eq!(
        location.to_json(),
        r#"{"organizationId":"org/1","projectId":"p 1","apiKeyId":"k1"}"#
    );
    assert_eq!(Location::parse(&location.to_json()), Some(location.clone()));
    assert_eq!(
        Location::parse(r#"{"organizationId":"o","projectId":"","apiKeyId":"k"}"#),
        None
    );
    assert_eq!(
        location.issue_url("https://bigmodel.cn"),
        "https://bigmodel.cn/api/biz/v1/organization/org%2F1/projects/p%201/api_keys/k1/access_tokens"
    );
}
