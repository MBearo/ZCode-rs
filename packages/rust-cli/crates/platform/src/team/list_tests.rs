//! 团队列表（platform.md §9.1，工作线 F49）：每个 App 夹具的回复对本地假服务，列出的团队项目与 App 的
//! `getEnterprisePricing` + 输入框团队行（`buildComposerPlanGroups`）逐项相同；App 读取失败的情形本工程是 unknown
//! （golden 由 `scripts/rust-cli/team-plan-golden.mts` 生成）。

use std::sync::Arc;

use rust_cli_net::{Egress, NetworkPolicy};
use serde_json::{Value, json};

use super::{TeamPlan, fetch};
use crate::fake_server_tests::{Reply, Server};
use crate::token::http::Http;

fn scripted(script: &Value) -> Reply {
    Reply::json(script["status"].as_u64().unwrap() as u16, &script["body"])
}

fn as_app(plans: &[TeamPlan]) -> Value {
    plans
        .iter()
        .map(|plan| {
            json!({
                "productId": plan.id.product_id,
                "organizationId": plan.id.organization_id,
                "projectId": plan.id.project_id,
                "name": plan.name,
            })
        })
        .collect()
}

#[tokio::test]
async fn team_lists_match_the_app() {
    let golden = crate::test_support::golden();
    let cases = golden["teamLists"].as_object().unwrap();
    assert!(cases.len() >= 15);
    for (name, case) in cases {
        let script = case["script"].clone();
        let server = Server::start(move |request| match request.path.as_str() {
            "/api/biz/subscription/enterprise/v2/pricing" => scripted(&script["pricing"]),
            "/api/biz/customer/getCustomerInfo" => scripted(&script["customer"]),
            _ => Reply::json(404, &json!({})),
        })
        .await;
        let http = Http::new(Arc::new(
            Egress::new(NetworkPolicy::default()).await.unwrap(),
        ));
        let token = if case["family"] == "zai" {
            "jwt-zai"
        } else {
            "at-big"
        };
        let (plans, _customer) = fetch(&http, &server.url(), token).await;
        let ours = plans.as_deref().map_or(Value::Null, as_app);
        assert_eq!(ours, case["rows"], "{name}");
        // pricing 用裸 token（两个 family 相同），与 App 的请求头一致。
        let pricing = server
            .requests()
            .into_iter()
            .find(|request| request.path.ends_with("/pricing"))
            .unwrap();
        assert_eq!(
            pricing.header("authorization"),
            case["pricingRequest"]["headers"]["authorization"].as_str(),
            "{name}"
        );
        // customerInfo 与 pricing 同时取（D-F49-1）：这一轮的权益核对也用它。
        assert_eq!(
            server.count("/api/biz/customer/getCustomerInfo"),
            1,
            "{name}"
        );
    }
}
