// 语义复制自 packages/services/src/model-provider/codingPlanProviderAvailability.ts 的
// `validateSelectedTeamPlanAvailability`、`classifyAvailabilityError`，与 packages/services/src/bigmodel/
// codingPlanEntitlement.ts 的 `fetchTeamCodingPlanEntitlement`（staging 699b62b8d6，工作线 F47）。
//! 一个团队项目的权益（platform.md §9.2）：customerInfo 核对这个项目，再查团队订阅详情。
//!
//! 工作线 F49：customerInfo 由这一轮的团队列表一起取（`list::fetch`），各项目共用一份（D-F49-1）；订阅详情各项目
//! 同时发出。判定表与 F47 相同。

use rust_cli_kernel_api::model::ModelConnection;
use serde_json::Value;

use super::list::Customer;
use super::{Availability, Reason};
use crate::token::Family;
use crate::token::http::{Auth, Fault, Http, ORGANIZATION_HEADER, PROJECT_HEADER, Reply};

/// 订阅详情请求的输入（未连接的情形调用方先排除）。
pub(crate) struct Detail<'a> {
    pub origin: &'a str,
    pub family: Family,
    pub team: &'a ModelConnection,
    pub token: &'a str,
    /// customerInfo 里找到的组织 ID（去空白）。
    pub organization: &'a str,
}

/// HTTP 的失败：401 / 403 是鉴权失败，其余（含 404、非 JSON、网络）是 unknown（App `classifyAvailabilityError`）。
fn classify(fault: Fault) -> Availability {
    match fault {
        Fault::Unauthorized | Fault::Forbidden => Availability::Unavailable(Reason::AuthFailed),
        Fault::Transient | Fault::Failed => Availability::Unknown,
    }
}

/// 用这一轮的 customerInfo 核对项目：找到时给出组织 ID（去空白），否则给出结论。
pub(crate) fn verify(customer: &Customer, team: &ModelConnection) -> Result<String, Availability> {
    let customer = customer.as_ref().map_err(|availability| *availability)?;
    if !business_success(customer) {
        return Err(Availability::Unknown);
    }
    let Some(organizations) = customer["data"]["organizations"].as_array() else {
        return Err(Availability::Unknown);
    };
    find_team_project(organizations, team)?.ok_or(Availability::Unavailable(Reason::NotEntitled))
}

/// 团队订阅详情（bigmodel 用裸 token，zai 用 Bearer；两个团队头）。
pub(crate) async fn detail(http: &Http, input: Detail<'_>) -> Availability {
    let auth = match input.family {
        Family::Zai => Auth::Bearer(input.token),
        Family::Bigmodel => Auth::Raw(input.token),
    };
    let extra = [
        (ORGANIZATION_HEADER, input.organization),
        (PROJECT_HEADER, input.team.project_id.as_str()),
    ];
    let reply = http
        .get_with(
            format!(
                "{}/api/biz/team/subscribe/product/querySubscribeDetail",
                input.origin
            ),
            auth,
            &extra,
        )
        .await;
    match reply {
        Ok(Reply::Json(payload)) => entitlement(&payload),
        Ok(Reply::NotFound) => Availability::Unknown,
        Err(fault) => classify(fault),
    }
}

/// customerInfo 的信封（App `isSuccessfulBusinessEnvelope`）：`success` 不是 false，`code` 缺省或是数字 0 / 200。
fn business_success(payload: &Value) -> bool {
    let code_ok = match payload.get("code") {
        None => true,
        Some(Value::Number(number)) => matches!(number.as_f64(), Some(n) if n == 0.0 || n == 200.0),
        Some(_) => false,
    };
    payload.get("success") != Some(&Value::Bool(false)) && code_ok
}

/// 所选组织下项目 ID 相同、类型为 `"2"` 的团队项目；找到时给出组织 ID（去空白）。组织或项目的形状不对时，App
/// 在遍历里抛错，结果是 unknown。
fn find_team_project(
    organizations: &[Value],
    team: &ModelConnection,
) -> Result<Option<String>, Availability> {
    let text = |value: &Value| -> Result<Option<String>, Availability> {
        match value {
            Value::Null => Ok(None),
            Value::String(text) => Ok(Some(rust_cli_base::js::string::trim(text).to_owned())),
            _ => Err(Availability::Unknown),
        }
    };
    for organization in organizations {
        if !organization.is_object() {
            return Err(Availability::Unknown);
        }
        let organization_id = text(&organization["organizationId"])?.unwrap_or_default();
        if organization_id != team.organization_id {
            continue;
        }
        let projects = match &organization["projects"] {
            Value::Null => continue,
            Value::Array(projects) => projects,
            _ => return Err(Availability::Unknown),
        };
        for project in projects {
            if !project.is_object() {
                return Err(Availability::Unknown);
            }
            let kind = match &project["projectType"] {
                Value::Null => String::new(),
                Value::String(kind) => kind.clone(),
                Value::Number(number) => {
                    rust_cli_base::js::json::js_double(number.as_f64().unwrap_or(f64::NAN))
                }
                Value::Bool(flag) => flag.to_string(),
                // `String(对象)` 不会是 "2"（数组的写法不考虑）。
                Value::Array(_) | Value::Object(_) => String::new(),
            };
            if text(&project["projectId"])?.as_deref() == Some(team.project_id.as_str())
                && rust_cli_base::js::string::trim(&kind) == "2"
                && !organization_id.is_empty()
            {
                return Ok(Some(organization_id));
            }
        }
    }
    Ok(None)
}

/// 团队订阅详情的判定表（App `fetchTeamCodingPlanEntitlement`）。
fn entitlement(payload: &Value) -> Availability {
    let Some(data) = successful_data(payload) else {
        return Availability::Unknown;
    };
    let Some(object) = data.as_object() else {
        return Availability::Unknown;
    };
    let Some(Value::Bool(has_subscription)) = object.get("hasSubscription") else {
        return Availability::Unknown;
    };
    let optional = [
        "status",
        "memberGrantStatus",
        "productId",
        "productName",
        "subscribeEndTime",
        "subscribePeriod",
    ];
    if optional.iter().any(|name| {
        object
            .get(*name)
            .is_some_and(|value| !value.is_null() && !value.is_string())
    }) {
        return Availability::Unknown;
    }
    if !has_subscription {
        return Availability::Unavailable(Reason::NotEntitled);
    }
    let status = object.get("status").and_then(Value::as_str);
    let grant = object.get("memberGrantStatus").and_then(Value::as_str);
    match (status, grant) {
        // 实际接口过期套餐仍返回 hasSubscription=true；存在订阅记录不等于当前有效。
        (Some("EXPIRED"), _) => Availability::Unavailable(Reason::Expired),
        (Some("EFFECTIVE"), Some("UNASSIGNED")) => Availability::Unavailable(Reason::Unassigned),
        (Some("EFFECTIVE"), Some("VALID")) => Availability::Available,
        // 未验证的新枚举保持未知，不能擅自解释成无权益。
        _ => Availability::Unknown,
    }
}

/// zod `{code?: number, success?: boolean, data?: unknown}`，成功时的 `data`（App `readSuccessfulData`；个人订阅列表
/// 也用它，`personal/check.rs`）。
pub(crate) fn successful_data(payload: &Value) -> Option<&Value> {
    let object = payload.as_object()?;
    let code = match object.get("code") {
        None => None,
        Some(Value::Number(number)) => Some(number.as_f64()?),
        Some(_) => return None,
    };
    let success = match object.get("success") {
        None => None,
        Some(Value::Bool(flag)) => Some(*flag),
        Some(_) => return None,
    };
    if success == Some(false) || code.is_some_and(|code| code != 0.0 && code != 200.0) {
        return None;
    }
    object.get("data")
}
