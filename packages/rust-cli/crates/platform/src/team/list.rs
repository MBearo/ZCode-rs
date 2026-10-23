// 语义复制自（staging 699b62b8d6）：packages/services/src/coding-plan-subscription/
// bigmodelCodingPlanSubscriptionProvider.ts 的 `getEnterprisePricing`、`enrichEnterprisePricingTeamProjects`、
// `resolveBigModelTeamProjects`、`resolveFallbackEnterpriseTeamPlanProduct`、`resolveProductTeamProjects`（zai 是同名
// 子类，只换 host 与 token），packages/ui/src/lib/composerPlanGroups.ts 的团队行与 teamPlanDisplayName.ts。
//! 团队列表（platform.md §9.1，工作线 F49）：pricing（必要时用 customerInfo 补全）→ 已订阅产品下的团队项目。
//!
//! 照 App 在 JSON 上做（补全是对象展开，显示名回退到产品上的组织名，这些细节都跟着对象走）；App 在哪里抛
//! TypeError、读取失败，这里就是 unknown。App 列出时预建团队 key、按 `apiKeyStatus` 筛，本工程不预建（D-F49-2）。

use rust_cli_kernel_api::model::ModelConnection;
use serde_json::{Map, Value};

use super::{Availability, Reason};
use crate::token::http::{Auth, CUSTOMER_INFO_PATH, Fault, Http, Reply};

/// 列出的一个团队项目：选择用的连接与团队显示名。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TeamPlan {
    pub id: ModelConnection,
    pub name: String,
}

/// 这一轮取到的 customerInfo：整个响应体，或按权益判定分好类的失败（401 / 403 是鉴权失败，其余 unknown）。
pub(crate) type Customer = Result<Value, Availability>;

/// JS 里会抛 TypeError 的地方（`x?.trim()` 遇到非字符串、遍历非数组）。
struct TypeError;

/// 同时取 pricing 与 customerInfo（D-F49-1），算出团队列表；列表读取失败时为 None（unknown）。customerInfo 原样
/// 给出，这一轮的权益核对用它。
pub(crate) async fn fetch(
    http: &Http,
    origin: &str,
    token: &str,
) -> (Option<Vec<TeamPlan>>, Customer) {
    let (pricing, customer) = tokio::join!(
        http.get(
            format!("{origin}/api/biz/subscription/enterprise/v2/pricing"),
            Auth::Raw(token),
        ),
        customer_info(http, origin, token),
    );
    let plans = match pricing {
        Ok(Reply::Json(payload)) => plans(&payload, &customer).ok(),
        Ok(Reply::NotFound) | Err(_) => None,
    };
    (plans, customer)
}

/// customerInfo：两个 family 都用裸 token（App `createZaiLoginAuthHeaders` / `createBigModelLoginAuthHeaders`）。
pub(crate) async fn customer_info(http: &Http, origin: &str, token: &str) -> Customer {
    let reply = http
        .get(format!("{origin}{CUSTOMER_INFO_PATH}"), Auth::Raw(token))
        .await;
    match reply {
        Ok(Reply::Json(value)) => Ok(value),
        Ok(Reply::NotFound) => Err(Availability::Unknown),
        Err(Fault::Unauthorized | Fault::Forbidden) => {
            Err(Availability::Unavailable(Reason::AuthFailed))
        }
        Err(Fault::Transient | Fault::Failed) => Err(Availability::Unknown),
    }
}

/// pricing 的信封（App `unwrapEnvelope`）：`code` 缺省或等于 200、`success` 不是 false、`data` 不为空。
fn unwrap_envelope(payload: &Value) -> Option<&Value> {
    let code_ok = match payload.get("code") {
        None => true,
        Some(Value::Number(number)) => number.as_f64() == Some(200.0),
        Some(_) => false,
    };
    let data = payload.get("data").filter(|data| !data.is_null())?;
    (code_ok && payload.get("success") != Some(&Value::Bool(false))).then_some(data)
}

/// `x?.trim()`：缺省或 null 是空，字符串去空白，其他类型是 TypeError。
fn trimmed(value: &Value) -> Result<String, TypeError> {
    match value {
        Value::Null => Ok(String::new()),
        Value::String(text) => Ok(rust_cli_base::js::string::trim(text).to_owned()),
        _ => Err(TypeError),
    }
}

/// `xs ?? []` 后遍历：缺省或 null 是空，数组照遍历，其他类型是 TypeError。
fn items(value: &Value) -> Result<&[Value], TypeError> {
    match value {
        Value::Null => Ok(&[]),
        Value::Array(items) => Ok(items),
        _ => Err(TypeError),
    }
}

/// 读属性：null 上读属性是 TypeError；别的非对象值读出来是 undefined（这里是 Null）。
fn object(value: &Value) -> Result<&Value, TypeError> {
    if value.is_null() {
        Err(TypeError)
    } else {
        Ok(value)
    }
}

fn subscribed(product: &Value) -> bool {
    product["subscribed"] == Value::Bool(true)
}

/// `String(x ?? "").trim()`：数字按 JS 的写法，对象与数组不会是 "2"。
fn project_type(value: &Value) -> String {
    let text = match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Number(number) => {
            rust_cli_base::js::json::js_double(number.as_f64().unwrap_or(f64::NAN))
        }
        Value::Bool(flag) => flag.to_string(),
        Value::Array(_) | Value::Object(_) => String::new(),
    };
    rust_cli_base::js::string::trim(&text).to_owned()
}

fn plans(payload: &Value, customer: &Customer) -> Result<Vec<TeamPlan>, TypeError> {
    let data = unwrap_envelope(payload).ok_or(TypeError)?;
    let Value::Array(products) = &data["productList"] else {
        return Err(TypeError);
    };
    if products.is_empty() {
        return Ok(Vec::new());
    }
    for product in products {
        object(product)?;
    }
    let has_subscribed = products.iter().any(subscribed);
    let mut missing_context = false;
    for product in products.iter().filter(|product| subscribed(product)) {
        missing_context |= trimmed(&product["projectId"])?.is_empty()
            || trimmed(&product["organizationId"])?.is_empty();
    }
    if has_subscribed && !missing_context {
        return project(products);
    }
    // 补全要 customerInfo；它读取失败时 App 整个 pricing 读取失败。
    let customer = customer.as_ref().map_err(|_| TypeError)?;
    let team_projects = team_projects(&customer["data"])?;
    if team_projects.is_empty() {
        return project(products);
    }
    let fallback = if has_subscribed {
        Value::Null
    } else {
        fallback_product(products)["productId"].clone()
    };
    let mut enriched = Vec::with_capacity(products.len());
    for product in products {
        let enrich =
            subscribed(product) || (!fallback.is_null() && product["productId"] == fallback);
        if !enrich {
            enriched.push(product.clone());
            continue;
        }
        let matched = matching(product, &team_projects)?;
        let Some(primary) = matched.first() else {
            enriched.push(product.clone());
            continue;
        };
        // 企业 pricing 的已订阅商品只给 productId / tier，或只给组织；按机构匹配 customerInfo 的团队项目补出连接。
        let mut next = product.as_object().cloned().unwrap_or_default();
        next.insert("organizationId".into(), primary["organizationId"].clone());
        let name = match &primary["organizationName"] {
            Value::Null => product["organizationName"].clone(),
            name => name.clone(),
        };
        next.insert("organizationName".into(), name);
        next.insert("projectId".into(), primary["projectId"].clone());
        next.insert("teamProjects".into(), Value::Array(matched.clone()));
        next.insert("subscribed".into(), Value::Bool(true));
        enriched.push(Value::Object(next));
    }
    project(&enriched)
}

/// customerInfo 里 `projectType == "2"` 的团队项目（App `resolveBigModelTeamProjects`），ID 去空白。
fn team_projects(data: &Value) -> Result<Vec<Value>, TypeError> {
    let mut out = Vec::new();
    // `customerInfo?.organizations`：data 为空时没有组织。
    let organizations = if data.is_null() {
        &[][..]
    } else {
        items(&data["organizations"])?
    };
    for organization in organizations {
        let organization = object(organization)?;
        let organization_id = trimmed(&organization["organizationId"])?;
        if organization_id.is_empty() {
            continue;
        }
        for project in items(&organization["projects"])? {
            let project = object(project)?;
            let project_id = trimmed(&project["projectId"])?;
            if project_id.is_empty() || project_type(&project["projectType"]) != "2" {
                continue;
            }
            let mut entry = Map::new();
            entry.insert("organizationId".into(), organization_id.clone().into());
            entry.insert(
                "organizationName".into(),
                organization["organizationName"].clone(),
            );
            entry.insert("projectId".into(), project_id.into());
            entry.insert("projectName".into(), project["projectName"].clone());
            out.push(Value::Object(entry));
        }
    }
    Ok(out)
}

/// 没有已订阅产品时补全的那个（App `resolveFallbackEnterpriseTeamPlanProduct`：分数最高、同分取先出现的）。
fn fallback_product(products: &[Value]) -> &Value {
    let score = |product: &Value| {
        let mut score = 0;
        if product["tier"] == "PRO" {
            score += 100;
        }
        if product["subscribeMode"] == "ONE_TIME" {
            score += 20;
        }
        if product["subscribePeriod"] == "MONTHLY" {
            score += 10;
        }
        score
    };
    let mut best = &products[0];
    for product in &products[1..] {
        if score(product) > score(best) {
            best = product;
        }
    }
    best
}

/// 产品自己的组织 / 项目（有就要相等）筛出的团队项目（App `resolveProductTeamProjects`）。
fn matching(product: &Value, team_projects: &[Value]) -> Result<Vec<Value>, TypeError> {
    let organization = trimmed(&product["organizationId"])?;
    let project = trimmed(&product["projectId"])?;
    Ok(team_projects
        .iter()
        .filter(|candidate| {
            organization.is_empty() || candidate["organizationId"] == organization.as_str()
        })
        .filter(|candidate| project.is_empty() || candidate["projectId"] == project.as_str())
        .cloned()
        .collect())
}

/// 输入框的团队行（App `buildComposerPlanGroups`）：已订阅、productId 非空的产品，它的每个团队项目（没有时是产品
/// 自己）；组织、项目、显示名都非空，`apiKeyStatus` 不是 unavailable；同一个（产品，组织，项目）只列一次。
fn project(products: &[Value]) -> Result<Vec<TeamPlan>, TypeError> {
    let mut out: Vec<TeamPlan> = Vec::new();
    for product in products.iter().filter(|product| subscribed(product)) {
        let Value::String(product_id) = &product["productId"] else {
            return Err(TypeError);
        };
        let product_id = rust_cli_base::js::string::trim(product_id);
        if product_id.is_empty() {
            continue;
        }
        let own = [product.clone()];
        let entries = match &product["teamProjects"] {
            Value::Array(entries) if !entries.is_empty() => entries.as_slice(),
            _ => &own[..],
        };
        for entry in entries {
            let entry = object(entry)?;
            let organization_id = trimmed(&entry["organizationId"])?;
            let project_id = trimmed(&entry["projectId"])?;
            // App `formatTeamPlanDisplayName`：组织名去空白，项目上没有时取产品上的。
            let name = match &entry["organizationName"] {
                Value::Null => trimmed(&product["organizationName"])?,
                name => trimmed(name)?,
            };
            let status = match &entry["apiKeyStatus"] {
                Value::Null => &product["apiKeyStatus"],
                status => status,
            };
            if organization_id.is_empty()
                || project_id.is_empty()
                || name.is_empty()
                || status == "unavailable"
            {
                continue;
            }
            let id = ModelConnection {
                product_id: product_id.to_owned(),
                organization_id,
                project_id,
            };
            if out.iter().all(|plan| plan.id != id) {
                out.push(TeamPlan { id, name });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
#[path = "list_tests.rs"]
mod tests;
