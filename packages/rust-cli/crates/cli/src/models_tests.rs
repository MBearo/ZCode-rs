use rust_cli_base::hosts::{ANTHROPIC_API, ANTHROPIC_API_V1, ZAI_ANTHROPIC};
use rust_cli_base::models::{GLM_5, GLM_5_3, GPT_5_4};
use rust_cli_kernel_api::provider::{CatalogModel, WireProtocol};

use super::*;

fn secret(value: &str) -> Option<RedactedString> {
    Some(RedactedString::new(value))
}

#[test]
fn the_anthropic_endpoint_comes_from_the_environment_snapshot() {
    assert_eq!(anthropic_endpoint(None, None), None);
    assert_eq!(
        anthropic_endpoint(Some("http://x".to_owned()), secret("")),
        None
    );
    assert_eq!(
        anthropic_endpoint(None, secret("sk-ant-1")),
        Some(AnthropicEndpoint {
            base_url: ANTHROPIC_API.to_owned(),
            api_key: RedactedString::new("sk-ant-1"),
        })
    );
    assert_eq!(
        anthropic_endpoint(Some("  ".to_owned()), secret("k")).map(|endpoint| endpoint.base_url),
        Some(ANTHROPIC_API.to_owned())
    );
    assert_eq!(
        anthropic_endpoint(Some(" http://127.0.0.1:9/proxy ".to_owned()), secret("k"))
            .map(|endpoint| endpoint.base_url),
        Some("http://127.0.0.1:9/proxy".to_owned())
    );
}

fn entry(provider: &str, model: &str) -> CatalogModel {
    CatalogModel {
        capabilities: Default::default(),
        provider: provider.to_owned(),
        provider_name: provider.to_owned(),
        model: model.to_owned(),
        protocol: WireProtocol::OpenAiChatCompletions,
        reasoning_levels: vec!["low".to_owned(), "high".to_owned()],
        context_window: 1,
        max_output_tokens: 1,
        connection: None,
    }
}

fn selection(provider: &str, model: &str, reasoning: Option<&str>) -> ModelSelection {
    ModelSelection {
        provider: provider.to_owned(),
        model: model.to_owned(),
        reasoning: reasoning.map(str::to_owned),
        connection: None,
    }
}

#[test]
fn the_model_flag_picks_from_the_catalog() {
    let catalog = ModelCatalog {
        revision: 1,
        models: vec![entry("p1", GLM_5), entry("p2", GLM_5), entry("p2", GPT_5_4)],
        default: Some(selection("p2", GPT_5_4, Some("low"))),
    };
    assert_eq!(
        pick(&catalog, None).unwrap(),
        selection("p2", GPT_5_4, Some("low"))
    );
    assert_eq!(
        pick(&catalog, Some(&format!("p2/{GLM_5}"))).unwrap(),
        selection("p2", GLM_5, None)
    );
    // 只给模型 ID 时取目录里第一个匹配。
    assert_eq!(
        pick(&catalog, Some(GLM_5)).unwrap(),
        selection("p1", GLM_5, None)
    );
    // 选中默认模型时沿用默认档位。
    assert_eq!(
        pick(&catalog, Some(GPT_5_4)).unwrap(),
        selection("p2", GPT_5_4, Some("low"))
    );
    let missing = pick(&catalog, Some(GLM_5_3)).unwrap_err();
    assert!(missing.contains(&format!("p2/{GPT_5_4}")), "{missing}");
    // 提示里是本工程读的名字（config.md §2.5），不提 Anthropic SDK 的变量。
    let none = pick(&ModelCatalog::default(), None).unwrap_err();
    assert!(none.contains("set ZCODE_API_KEY"), "{none}");
    assert!(!none.contains("ANTHROPIC"), "{none}");
}

/// 团队套餐的写法（cli-args.md §1.3，工作线 F49）：`[<团队>]` 选团队项目；多个团队时不默认取第一个；找不到时可用项
/// 的团队也写出来。
#[test]
fn the_model_flag_names_the_team() {
    use rust_cli_kernel_api::model::ModelConnection;
    use rust_cli_kernel_api::provider::CatalogConnection;
    let team = |project: &str, name: &str| {
        let mut entry = entry("t", "glm");
        entry.connection = Some(CatalogConnection {
            id: ModelConnection {
                product_id: "prod-1".to_owned(),
                organization_id: "org-t".to_owned(),
                project_id: project.to_owned(),
            },
            vendor: "BigModel".to_owned(),
            name: name.to_owned(),
        });
        entry
    };
    let catalog = ModelCatalog {
        revision: 1,
        models: vec![team("p-a", "A"), team("p-b", "B")],
        default: None,
    };
    let chosen = pick(&catalog, Some("t/glm[B]")).unwrap();
    assert_eq!(chosen.connection.unwrap().project_id, "p-b");
    assert_eq!(
        pick(&catalog, Some("t/glm")).unwrap_err(),
        "model t/glm matches several team plans; pass one of: t/glm[A], t/glm[B]"
    );
    assert_eq!(
        pick(&catalog, Some("x")).unwrap_err(),
        "model x is not configured (available: t/glm[A], t/glm[B])"
    );
    // 只有一个团队项目：不用写哪一个。
    let one = ModelCatalog {
        models: vec![team("p-a", "A")],
        ..catalog
    };
    assert_eq!(
        pick(&one, Some("t/glm"))
            .unwrap()
            .connection
            .unwrap()
            .project_id,
        "p-a"
    );
}

/// 显式覆盖的模型（model.md §6，用户决定 2026-10-20，修复线 F42）：`--model` → `ZCODE_MODEL` → 个人 Provider 配置的
/// `defaultModelSelection.modelId`；都没有时报错提示 `--model`，没有写死的缺省模型。
#[test]
fn the_explicit_model_falls_back_to_the_default_model_selection() {
    let pick = |flag: Option<&str>, env: Option<&str>, configured: Option<&str>| {
        explicit_model(
            flag.map(str::to_owned),
            env.map(str::to_owned),
            configured.map(str::to_owned),
        )
    };
    assert_eq!(pick(Some("a"), Some("b"), Some("c")).unwrap(), "a");
    assert_eq!(pick(None, Some("b"), Some("c")).unwrap(), "b");
    assert_eq!(pick(None, None, Some("c")).unwrap(), "c");
    assert_eq!(
        pick(None, None, None).unwrap_err(),
        "ZCODE_API_KEY is set: pass --model or set ZCODE_MODEL"
    );
}

#[test]
fn the_rule_url_keeps_the_old_v1_rule() {
    // F55：规则改由 model 的 `anthropic_v1_base_url` 给出；结果与原来 cli 自己的写法逐字相同。
    let old = |base_url: &str| {
        let base = base_url.trim().trim_end_matches('/');
        if base.to_ascii_lowercase().ends_with("/v1") {
            base.to_owned()
        } else {
            format!("{base}/v1")
        }
    };
    let with_slash = format!("{ANTHROPIC_API}/");
    for input in [
        ANTHROPIC_API,
        with_slash.as_str(),
        " https://gw.example.com/V1/ ",
        ZAI_ANTHROPIC,
        "http://127.0.0.1:9//",
        "",
    ] {
        assert_eq!(rule_base_url(input), old(input), "{input:?}");
    }
    assert_eq!(OFFICIAL_RULE_URL, ANTHROPIC_API_V1);
}

/// `--effort`（cli-args.md §1.4）：目录里的模型按它的档位取舍，显式覆盖原样写；没给时不改。
#[test]
fn the_effort_flag_sets_the_new_session_level() {
    let catalog = ModelCatalog {
        revision: 1,
        models: vec![entry("p1", GLM_5)],
        default: None,
    };
    let chosen = selection("p1", GLM_5, None);
    assert_eq!(
        with_effort(chosen.clone(), Some("high"), Some(&catalog)).reasoning,
        Some("high".to_owned())
    );
    assert_eq!(
        with_effort(chosen.clone(), Some("max"), Some(&catalog)).reasoning,
        Some("high".to_owned())
    );
    assert_eq!(
        with_effort(chosen.clone(), Some("medium"), Some(&catalog)).reasoning,
        None
    );
    assert_eq!(with_effort(chosen.clone(), None, Some(&catalog)), chosen);
    // 不在目录里的选择（不该出现）：没有可用的档位，不改。
    let unknown = selection("p9", "x", Some("low"));
    assert_eq!(
        with_effort(unknown.clone(), Some("high"), Some(&catalog)),
        unknown
    );
    assert_eq!(
        with_effort(selection("anthropic", GLM_5_3, None), Some("max"), None).reasoning,
        Some("max".to_owned())
    );
}
