// 语义复制自 packages/services/src/model-provider/codingPlanProviderAvailability.ts 的
// `validateSubscriptionListAvailability`、`classifyAvailabilityError`，与 packages/services/src/bigmodel/
// codingPlanEntitlement.ts 的 `fetchPersonalCodingPlanEntitlement`、`isActivePersonalCodingPlan`（feat/rust-cli
// 2827169cbc，工作线 F50）。
//! 个人 Coding Plan 的订阅查询（platform.md §5.7）：用个人项目令牌查 `/api/biz/subscription/list`。

use serde_json::Value;

use crate::team::{Availability, Reason, successful_data};
use crate::token::http::{Auth, Fault, Http, Reply};

/// `GET {origin}/api/biz/subscription/list`，`Authorization: <项目令牌>`（不带 Bearer，App 相同）。
pub(crate) async fn subscription(http: &Http, origin: &str, project_token: &str) -> Availability {
    let url = format!("{origin}/api/biz/subscription/list");
    match http.get(url, Auth::Raw(project_token)).await {
        Ok(Reply::Json(payload)) => entitlement(&payload),
        // App `classifyAvailabilityError`：401 / 403 是鉴权失败，其余失败（含 404、非 JSON、网络）是 unknown。
        Err(Fault::Unauthorized | Fault::Forbidden) => {
            Availability::Unavailable(Reason::AuthFailed)
        }
        Ok(Reply::NotFound) | Err(Fault::Transient | Fault::Failed) => Availability::Unknown,
    }
}

/// 订阅列表的判定（App `fetchPersonalCodingPlanEntitlement`）：有一项合 schema 且是有效的 Coding 订阅时 available；
/// 没有时，若有不合 schema 的 Coding 产品是 unknown（疑似 Coding 的坏数据不当作没开通），否则 unavailable。
pub(crate) fn entitlement(payload: &Value) -> Availability {
    let Some(Value::Array(items)) = successful_data(payload) else {
        return Availability::Unknown;
    };
    let mut malformed = false;
    for item in items {
        match parse(item) {
            Some(subscription) if subscription.active() => return Availability::Available,
            Some(_) => {}
            None => malformed |= coding_product(item),
        }
    }
    if malformed {
        Availability::Unknown
    } else {
        Availability::Unavailable(Reason::NotEntitled)
    }
}

/// zod `personalSchema` 读出的一项（只留判定要用的字段）。
struct Subscription<'a> {
    item: &'a Value,
    status: &'a str,
    in_current_period: bool,
}

impl Subscription<'_> {
    /// App `isActivePersonalCodingPlan`。
    fn active(&self) -> bool {
        coding_product(self.item) && self.status == "VALID" && self.in_current_period
    }
}

/// zod `z.object({...})`（不严格，多余字段不管）：`status` 字符串、`inCurrentPeriod` 布尔必有；`productId`、
/// `productName`、`nextRenewTime`、`valid`、`billingCycle` 有就是字符串，`autoRenew` 有就是数字或布尔（null 不合法）。
fn parse(item: &Value) -> Option<Subscription<'_>> {
    let object = item.as_object()?;
    let status = object.get("status")?.as_str()?;
    let in_current_period = object.get("inCurrentPeriod")?.as_bool()?;
    let strings = [
        "productId",
        "productName",
        "nextRenewTime",
        "valid",
        "billingCycle",
    ];
    if strings
        .iter()
        .any(|name| object.get(*name).is_some_and(|value| !value.is_string()))
    {
        return None;
    }
    if object
        .get("autoRenew")
        .is_some_and(|value| !value.is_number() && !value.is_boolean())
    {
        return None;
    }
    Some(Subscription {
        item,
        status,
        in_current_period,
    })
}

/// App `isCodingPlanProduct`：对象（不是数组），`productId` 或 `productName` 是字符串、转小写后含 `coding`。
fn coding_product(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    ["productId", "productName"].iter().any(|name| {
        object
            .get(*name)
            .and_then(Value::as_str)
            .is_some_and(|text| text.to_lowercase().contains("coding"))
    })
}

#[cfg(test)]
#[path = "check_tests.rs"]
mod tests;
