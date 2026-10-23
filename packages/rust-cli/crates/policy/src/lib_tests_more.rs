//! 判决链的分支（续）：自动模式与分类器、没有人回答、会话规则、工具自身的检查、WebFetch。

use super::*;

struct Verdict(ClassifierVerdict);

#[async_trait]
impl AutoClassifier for Verdict {
    async fn classify(&self, request: ClassifierRequest) -> ClassifierVerdict {
        assert_eq!(request.tool, "Bash");
        self.0.clone()
    }
}

#[tokio::test]
async fn auto_mode_takes_the_accept_edits_fast_path_then_asks_the_classifier() {
    let blocked =
        PolicyGate::default().with_classifier(Arc::new(Verdict(ClassifierVerdict::Block {
            reason: "Pushes to the default branch".to_owned(),
        })));
    assert_eq!(
        bash(&blocked, PermissionMode::Auto, "touch x").await,
        allow()
    );
    assert_eq!(
        bash(&blocked, PermissionMode::Auto, "git push").await,
        deny(texts::classifier_blocked("Pushes to the default branch"))
    );
    let allowed =
        PolicyGate::default().with_classifier(Arc::new(Verdict(ClassifierVerdict::Allow)));
    assert_eq!(
        bash(&allowed, PermissionMode::Auto, "git push").await,
        allow()
    );
    // 安全路径不经分类器，仍然询问。
    let sensitive = decide(
        &allowed,
        PermissionMode::Auto,
        "Write",
        json!({"file_path": "/w/.zcode/settings.json"}),
    )
    .await;
    assert!(matches!(sensitive, PermissionDecision::Ask { .. }));
}

#[tokio::test]
async fn without_anyone_to_answer_questions_become_denials_with_their_text() {
    let mut base = rules(&[]);
    base.avoid_prompts = true;
    let gate = PolicyGate::new(base);
    assert_eq!(
        bash(&gate, PermissionMode::Default, "touch x").await,
        deny(TOUCH_INSIDE)
    );
}

#[tokio::test]
async fn the_tools_own_decision_goes_through_the_chain() {
    let gate = PolicyGate::new(rules(&[(RuleBehavior::Deny, "Custom")]));
    let own_allow = Named(
        "Other",
        PermissionDecision::Allow {
            updated_input: Some(json!({"x": 1})),
        },
    );
    assert_eq!(
        gate.decide(&own_allow, &json!({}), &context(PermissionMode::Default))
            .await,
        PermissionDecision::Allow {
            updated_input: Some(json!({"x": 1}))
        }
    );
    let own_deny = Named(
        "Other",
        PermissionDecision::Deny {
            message: "no".to_owned(),
        },
    );
    assert_eq!(
        gate.decide(
            &own_deny,
            &json!({}),
            &context(PermissionMode::BypassPermissions)
        )
        .await,
        deny("no")
    );
    let unknown = Named("Other", PermissionDecision::Passthrough);
    assert_eq!(
        gate.decide(
            &unknown,
            &json!({}),
            &context(PermissionMode::BypassPermissions)
        )
        .await,
        allow()
    );
    assert_eq!(
        ask_message(
            &gate
                .decide(&unknown, &json!({}), &context(PermissionMode::Default))
                .await
        ),
        "ZCode requested permissions to use Other, but you haven't granted it yet."
    );
    gate.replace_rules(rules(&[(RuleBehavior::Deny, "Other")]));
    assert_eq!(
        gate.decide(&unknown, &json!({}), &context(PermissionMode::Default))
            .await,
        deny("Permission to use Other has been denied.")
    );
}

#[tokio::test]
async fn web_fetch_domain_rules() {
    let gate = PolicyGate::new(rules(&[
        (RuleBehavior::Allow, "WebFetch(domain:example.com)"),
        (RuleBehavior::Deny, "WebFetch(domain:evil.example)"),
    ]));
    let fetch = |url: &str| json!({"url": url, "prompt": "p"});
    assert_eq!(
        decide(
            &gate,
            PermissionMode::Default,
            "WebFetch",
            fetch("https://example.com/a")
        )
        .await,
        allow()
    );
    assert_eq!(
        decide(
            &gate,
            PermissionMode::Default,
            "WebFetch",
            fetch("https://EVIL.example:443/")
        )
        .await,
        deny("WebFetch denied access to domain:evil.example.")
    );
    let PermissionDecision::Ask { suggestions, .. } = decide(
        &gate,
        PermissionMode::Default,
        "WebFetch",
        fetch("https://docs.rs/x"),
    )
    .await
    else {
        panic!("ask");
    };
    assert_eq!(suggestions[0].content.as_deref(), Some("domain:docs.rs"));
}

#[tokio::test]
async fn check_rules_reports_only_the_gates_hooks_cannot_bypass() {
    let gate = PolicyGate::new(rules(&[
        (RuleBehavior::Deny, "Bash(rm:*)"),
        (RuleBehavior::Ask, "Bash(git push:*)"),
    ]));
    let check = |command: &'static str| {
        let gate = &gate;
        async move {
            let input = json!({"command": command});
            gate.check_rules(
                &Named("Bash", PermissionDecision::Passthrough),
                &input,
                &context(PermissionMode::Default),
            )
            .await
        }
    };
    assert!(matches!(
        check("rm -rf x").await,
        Some(PermissionDecision::Deny { .. })
    ));
    assert!(matches!(
        check("git push").await,
        Some(PermissionDecision::Ask { .. })
    ));
    // 普通的询问（没有规则）不算：hook 的 allow 可以跳过它。
    assert_eq!(check("touch x").await, None);
    // 安全路径的询问不能跳过。
    let sensitive = gate
        .check_rules(
            &Named("Write", PermissionDecision::Passthrough),
            &json!({"file_path": "/w/.zcode/settings.json"}),
            &context(PermissionMode::Default),
        )
        .await;
    assert!(matches!(sensitive, Some(PermissionDecision::Ask { .. })));
}

/// Node 群协作会话（修复线 F2）：`scope` 之后这个会话不用本机项目规则集（LocalSettings），只用自己的规则集；
/// 别的会话不变。
#[tokio::test]
async fn a_scoped_session_uses_its_own_rules_instead_of_the_project_ones() {
    use rust_cli_kernel_api::permission::{ProjectRules, RuleBehavior, RuleSource};
    let mut base = PermissionRules::default();
    base.rules.extend(
        ProjectRules {
            allow: vec![rules::parse_rule("Bash(touch:*)")],
            ..ProjectRules::default()
        }
        .sourced(),
    );
    assert!(
        base.rules
            .iter()
            .all(|r| r.source == RuleSource::LocalSettings && r.behavior == RuleBehavior::Allow)
    );
    let gate = PolicyGate::new(base);
    assert_eq!(
        bash(&gate, PermissionMode::Default, "touch x").await,
        allow()
    );
    gate.scope(
        &SessionId::new("s1"),
        &ProjectRules {
            deny: vec![rules::parse_rule("Bash(rm:*)")],
            ..ProjectRules::default()
        },
    );
    assert_eq!(
        ask_message(&bash(&gate, PermissionMode::Default, "touch x").await),
        TOUCH_INSIDE
    );
    assert!(matches!(
        bash(&gate, PermissionMode::Default, "rm x").await,
        PermissionDecision::Deny { .. }
    ));
    // 快照本身不变：`/permissions` 与别的会话照旧看到项目规则。
    assert_eq!(gate.rules().rules.len(), 1);
    // 会话关闭（session-lifecycle.md §3.3）后规则集释放：再判决回到项目规则。
    gate.release(&SessionId::new("s1"));
    assert_eq!(
        bash(&gate, PermissionMode::Default, "touch x").await,
        allow()
    );
}

/// 修复线 F51b（shell-permissions.md §4.8）：询问的四句说 ZCode，与执行管线里没有规则时的
/// `ZCode requested permissions to use …` 一致。
#[test]
fn the_ask_texts_name_zcode() {
    let texts = [
        crate::texts::request_message("Bash", None),
        crate::texts::file_read_request("/w/a"),
        crate::texts::file_write_request("/w/a"),
        crate::texts::file_sensitive("/w/.git/config"),
    ];
    for text in &texts {
        assert!(
            text.starts_with("ZCode requested permissions to "),
            "{text}"
        );
    }
}
