// 语义复制自 packages/shared/src/project-access-token-client.ts（staging 699b62b8d6）的 `#resolveLocation`、
// `keyListUrl` 与项目位置的存放键（个人 non-team 与团队两种，工作线 F47）。
//! 项目位置（platform.md §5.4）：组织、项目与 key 的 ID。
//!
//! 解析一次后加密存进 `credentials.json`（键 [`storage_key`]），之后的进程直接读；签发回 404 时删掉。

use rust_cli_base::js::json::js_double;
use rust_cli_base::js::string::trim as js_trim;
use serde_json::{Value, json};

use super::TokenError;
use super::http::{
    Auth, CUSTOMER_INFO_PATH, Fault, Http, ORGANIZATION_HEADER, PROJECT_HEADER, Reply,
    envelope_data,
};
use crate::account::encode_uri_component;
use crate::token::Family;

const DEFAULT_ORGANIZATION_NAME: &str = "默认机构";
const DEFAULT_PROJECT_NAME: &str = "默认项目";
const KEY_NAME: &str = "zcode-api-key";
/// 团队项目的 key（App `BIGMODEL_TEAM_PLAN_API_KEY_NAME`）与类型。
const TEAM_KEY_NAME: &str = "zcode-team-api-key";
const TEAM_KEY_TYPE: u64 = 2;
/// `API_KEY_USAGE_SCENE.CODING_PLAN`。
pub(crate) const CODING_PLAN_USAGE_SCENE: u64 = 1;
/// 个人项目的选择规则（App 的缺省 `personalProjectSelection: "non-team"`，F47）。
const PERSONAL_SELECTION: &str = "non-team";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub organization_id: String,
    pub project_id: String,
    pub api_key_id: String,
}

/// 团队项目（连接选择里的组织与项目，platform.md §9.1）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct TeamProject {
    pub organization_id: String,
    pub project_id: String,
}

impl Location {
    /// 存放的值：`JSON.stringify({organizationId, projectId, apiKeyId})`。
    pub fn to_json(&self) -> String {
        json!({
            "organizationId": self.organization_id,
            "projectId": self.project_id,
            "apiKeyId": self.api_key_id,
        })
        .to_string()
    }

    /// 读回存放的值；不是三个非空字符串时为空（重新解析）。
    pub fn parse(saved: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(saved).ok()?;
        let field = |name: &str| {
            value[name]
                .as_str()
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        Some(Self {
            organization_id: field("organizationId")?,
            project_id: field("projectId")?,
            api_key_id: field("apiKeyId")?,
        })
    }

    /// 团队读回时组织与项目要与选择相同（App 相同；对不上就重新解析）。
    pub(crate) fn matches(&self, team: Option<&TeamProject>) -> bool {
        team.is_none_or(|team| {
            self.organization_id == team.organization_id && self.project_id == team.project_id
        })
    }

    /// `{origin}/api/biz/v1/organization/{org}/projects/{project}/api_keys`。
    pub fn key_list_url(origin: &str, organization: &str, project: &str) -> String {
        format!(
            "{origin}/api/biz/v1/organization/{}/projects/{}/api_keys",
            encode_uri_component(organization),
            encode_uri_component(project)
        )
    }

    /// 签发地址 `…/api_keys/{apiKeyId}/access_tokens`。
    pub fn issue_url(&self, origin: &str) -> String {
        format!(
            "{}/{}/access_tokens",
            Self::key_list_url(origin, &self.organization_id, &self.project_id),
            encode_uri_component(&self.api_key_id)
        )
    }
}

/// `{"organizationId","projectId"}`（App 的 `team` 对象，键序相同）或 `null`。
fn team_json(team: Option<&TeamProject>) -> Value {
    team.map_or(
        Value::Null,
        |team| json!({"organizationId": team.organization_id, "projectId": team.project_id}),
    )
}

/// 进程内缓存与单飞的范围：`JSON [账号, 团队 | null, 选择]`（App 的 `locationKey`）。
pub(crate) fn scope_key(account: &str, team: Option<&TeamProject>) -> String {
    let selection = team.map_or(json!(PERSONAL_SELECTION), |_| Value::Null);
    rust_cli_base::js::json::stringify(&json!([account, team_json(team), selection]))
}

/// `project-key-location:v1:` + `encodeURIComponent(JSON [origin, family, 账号, 团队 | null, 1, 选择])`：个人是
/// `[…, null, 1, "non-team"]`，团队是 `[…, {organizationId, projectId}, 1, null]`（App 的写法，F47；Node 独立
/// CLI 的个人位置是 `"default"`，键不同，platform.md §5.4）。
pub(crate) fn storage_key(
    origin: &str,
    family: Family,
    account: &str,
    team: Option<&TeamProject>,
) -> String {
    let selection = team.map_or(json!(PERSONAL_SELECTION), |_| Value::Null);
    let scope = json!([
        origin,
        family.as_str(),
        account,
        team_json(team),
        CODING_PLAN_USAGE_SCENE,
        selection
    ]);
    format!(
        "project-key-location:v1:{}",
        encode_uri_component(&rust_cli_base::js::json::stringify(&scope))
    )
}

/// 管理接口的 `data`；信封不对时是 `project_token_invalid_response`，请求失败是 `project_token_request_failed`。
async fn data(request: impl Future<Output = Result<Reply, Fault>>) -> Result<Value, TokenError> {
    let reply = request.await.map_err(|_| TokenError::RequestFailed)?;
    envelope_data(&reply)
        .cloned()
        .ok_or(TokenError::InvalidResponse)
}

/// customerInfo 里的一个组织：ID、名字与项目（ID、名字、类型）。
struct Organization {
    id: String,
    name: Option<String>,
    projects: Vec<Project>,
}

struct Project {
    id: String,
    name: Option<String>,
    /// `String(projectType ?? "").trim()`。
    kind: String,
}

/// customerInfo（zod `customerSchema`）：形状不对是 `project_token_invalid_response`。
fn organizations(customer: &Value) -> Result<Vec<Organization>, TokenError> {
    let invalid = TokenError::InvalidResponse;
    if !customer.is_object() {
        return Err(invalid);
    }
    let organizations = match customer.get("organizations") {
        None => Vec::new(),
        Some(Value::Array(items)) => items.clone(),
        Some(_) => return Err(invalid),
    };
    let mut parsed = Vec::new();
    for organization in &organizations {
        let id = organization["organizationId"].as_str().ok_or(invalid)?;
        let name = optional_text(organization, "organizationName")?;
        let projects = match organization.get("projects") {
            None => Vec::new(),
            Some(Value::Array(items)) => items.clone(),
            Some(_) => return Err(invalid),
        };
        let mut members = Vec::new();
        for project in &projects {
            let project_id = project["projectId"].as_str().ok_or(invalid)?;
            let kind = match project.get("projectType") {
                None | Some(Value::Null) => String::new(),
                Some(Value::String(text)) => text.clone(),
                Some(Value::Number(number)) => js_double(number.as_f64().unwrap_or(f64::NAN)),
                Some(_) => return Err(invalid),
            };
            members.push(Project {
                id: project_id.to_owned(),
                name: optional_text(project, "projectName")?,
                kind: js_trim(&kind).to_owned(),
            });
        }
        parsed.push(Organization {
            id: id.to_owned(),
            name,
            projects: members,
        });
    }
    Ok(parsed)
}

/// 个人项目（App 的 non-team）：去掉团队项目（类型 `"2"`）与没有组织 ID、没有项目的组织；组织名含“默认机构”
/// 的优先，否则第一个；项目名含“默认项目”的优先，否则第一个。
///
/// 修复线 F47：以前按 Node 独立 CLI 的 `default` 不排除团队项目，默认机构里的团队项目可能被当成个人项目，
/// 个人的 key 建进了团队项目。现在照 App 先排除团队项目。
fn pick_personal(organizations: Vec<Organization>) -> Result<(String, String), TokenError> {
    let personal: Vec<Organization> = organizations
        .into_iter()
        .map(|mut organization| {
            organization.projects.retain(|project| project.kind != "2");
            organization
        })
        .filter(|organization| !organization.id.is_empty() && !organization.projects.is_empty())
        .collect();
    let organization = personal
        .iter()
        .find(|o| {
            o.name
                .as_deref()
                .is_some_and(|name| name.contains(DEFAULT_ORGANIZATION_NAME))
        })
        .or_else(|| personal.first());
    let project = organization.and_then(|organization| {
        organization
            .projects
            .iter()
            .find(|p| {
                p.name
                    .as_deref()
                    .is_some_and(|name| name.contains(DEFAULT_PROJECT_NAME))
            })
            .or_else(|| organization.projects.first())
    });
    match (organization, project) {
        (Some(organization), Some(project)) if !project.id.is_empty() => {
            Ok((organization.id.clone(), project.id.clone()))
        }
        _ => Err(TokenError::ProjectUnavailable),
    }
}

/// 团队项目要在 customerInfo 里（组织与项目的 ID 去空白后相等；不看项目类型，App 相同）。
fn check_team(organizations: &[Organization], team: &TeamProject) -> Result<(), TokenError> {
    let trim = |text: &str| js_trim(text).to_owned();
    let (organization, project) = (trim(&team.organization_id), trim(&team.project_id));
    let found = organizations.iter().any(|candidate| {
        trim(&candidate.id) == organization
            && candidate
                .projects
                .iter()
                .any(|member| trim(&member.id) == project)
    });
    if found {
        Ok(())
    } else {
        Err(TokenError::ProjectUnavailable)
    }
}

/// 可选的字符串字段（zod `string().nullish()`）。
fn optional_text(object: &Value, name: &str) -> Result<Option<String>, TokenError> {
    match object.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(TokenError::InvalidResponse),
    }
}

/// key 条目（zod `keySchema`）：`apiKey` 去空白后非空，`name` 是字符串或空，`keyType` 是数字或空。给出 ID、名字与
/// 类型。
fn key_entry(entry: &Value) -> Option<(String, Option<String>, Option<f64>)> {
    let name = optional_text(entry, "name").ok()?;
    let key_type = match entry.get("keyType") {
        None | Some(Value::Null) => None,
        Some(Value::Number(number)) => number.as_f64(),
        Some(_) => return None,
    };
    let id = entry["apiKey"]
        .as_str()
        .map(js_trim)
        .filter(|id| !id.is_empty())?;
    Some((id.to_owned(), name, key_type))
}

/// 一次解析的输入。
pub(crate) struct Lookup<'a> {
    pub origin: &'a str,
    pub family: Family,
    pub login_token: &'a str,
    pub team: Option<&'a TeamProject>,
}

/// 解析项目位置：customerInfo →（个人 / 团队）key 列表 →（没有可用 key 时）创建。
pub(crate) async fn resolve(http: &Http, lookup: Lookup<'_>) -> Result<Location, TokenError> {
    let Lookup {
        origin,
        family,
        login_token,
        team,
    } = lookup;
    // customerInfo 要裸 token：Bearer 会被拒，误判为登录失效。
    let customer = data(http.get(
        format!("{origin}{CUSTOMER_INFO_PATH}"),
        Auth::Raw(login_token),
    ))
    .await?;
    let organizations = organizations(&customer)?;
    let (organization, project) = match team {
        Some(team) => {
            check_team(&organizations, team)?;
            (team.organization_id.clone(), team.project_id.clone())
        }
        None => pick_personal(organizations)?,
    };
    let url = Location::key_list_url(origin, &organization, &project);
    // 团队 key：两个 family 都用裸 token，带组织与项目头；个人 key 沿用既有格式（zai Bearer，bigmodel 裸 token）。
    let team_headers = [
        (ORGANIZATION_HEADER, organization.as_str()),
        (PROJECT_HEADER, project.as_str()),
    ];
    let (auth, extra): (Auth<'_>, &[(&str, &str)]) = match (team, family) {
        (Some(_), _) => (Auth::Raw(login_token), &team_headers),
        (None, Family::Zai) => (Auth::Bearer(login_token), &[]),
        (None, Family::Bigmodel) => (Auth::Raw(login_token), &[]),
    };
    let list = data(http.get_with(url.clone(), auth, extra)).await?;
    let Value::Array(entries) = list else {
        return Err(TokenError::InvalidResponse);
    };
    let name = if team.is_some() {
        TEAM_KEY_NAME
    } else {
        KEY_NAME
    };
    // 列表可能含无关坏条目。个人同名条目缺 ID 时不重复创建（报格式错误）；团队只复用合法、keyType 2 的条目。
    let existing = entries.iter().find(|entry| {
        if entry.get("name").is_none_or(|value| value != name) {
            return false;
        }
        team.is_none()
            || key_entry(entry)
                .is_some_and(|(_, _, key_type)| key_type == Some(TEAM_KEY_TYPE as f64))
    });
    let key = match existing {
        Some(entry) => entry.clone(),
        None => {
            let body = match team {
                Some(_) => json!({
                    "name": name,
                    "keyType": TEAM_KEY_TYPE,
                    "usageScene": CODING_PLAN_USAGE_SCENE,
                }),
                None => json!({"name": name, "usageScene": CODING_PLAN_USAGE_SCENE}),
            };
            data(http.post_with(url, auth, extra, &body)).await?
        }
    };
    let (api_key_id, key_name, key_type) = key_entry(&key).ok_or(TokenError::InvalidResponse)?;
    if team.is_some()
        && (key_name.as_deref() != Some(name) || key_type != Some(TEAM_KEY_TYPE as f64))
    {
        return Err(TokenError::InvalidResponse);
    }
    Ok(Location {
        organization_id: organization,
        project_id: project,
        api_key_id,
    })
}
