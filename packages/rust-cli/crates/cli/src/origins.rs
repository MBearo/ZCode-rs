//! 账号服务的地址（docs/specs/rust-cli/platform.md §5.4、§5.6）：按 Node 的环境变量规则解析。
//!
//! - ZCode 服务（CLI OAuth 登录）：`resolveRuntimeZCodeEndpointOrigin`——`ZCODE_ENV=test` 时是测试环境；
//!   `ZCODE_BASE_URL`、`ZCODE_ENDPOINT_ORIGIN`、分环境的 `ZCODE_PRODUCTION_BASE_URL` / `ZCODE_TEST_BASE_URL`
//!   依次覆盖；API 根是 `<origin>/api/v1`。
//! - BigModel：`resolveBigModelApiOrigin`——`BIGMODEL_API_BASE_URL`，其次分环境的
//!   `BIGMODEL_PRODUCTION_API_BASE_URL` / `BIGMODEL_TEST_API_BASE_URL`。
//! - Z.ai 业务与项目接口：App 的 `resolveZaiBusinessBaseUrl`——`ZAI_BUSINESS_BASE_URL`，其次分环境的
//!   `ZAI_PRODUCTION_BUSINESS_BASE_URL` / `ZAI_TEST_BUSINESS_BASE_URL`，缺省 `https://api.z.ai`（测试环境
//!   `https://api.z.ai`）。工作线 F47：z.ai 的 token 与团队都照 App，以前按 Node 的换证 adapter 固定
//!   `https://api.z.ai`（生产缺省相同）。

use rust_cli_platform::AccountOrigins;

const ZCODE_ORIGIN: &str = "https://zcode.z.ai";
const ZCODE_TEST_ORIGIN: &str = "https://zcode.z.ai";
const BIGMODEL_ORIGIN: &str = "https://bigmodel.cn";
const BIGMODEL_TEST_ORIGIN: &str = "https://bigmodel.cn";
const ZAI_API: &str = "https://api.z.ai";
const ZAI_TEST_API: &str = "https://api.z.ai";

/// Node `normalizeZCodeEndpointOrigin`：http(s) URL 的 origin。
fn origin(name: &str, value: &str) -> Result<String, String> {
    let invalid = || format!("{name} must be an http(s) URL, got {value}");
    let url = url::Url::parse(value.trim()).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid());
    }
    Ok(url.origin().ascii_serialization())
}

/// Node `resolveRuntimeZCodeEnv`：`ZCODE_ENV`（去掉首尾空白）不分大小写等于 `test` 时是测试环境。`env` 同下。
pub(crate) fn is_test(env: &impl Fn(&str) -> Option<String>) -> bool {
    env("ZCODE_ENV").is_some_and(|value| value.eq_ignore_ascii_case("test"))
}

/// 第一个有值的变量（名字与值）。
fn first(
    env: &impl Fn(&str) -> Option<String>,
    names: &[&'static str],
) -> Option<(&'static str, String)> {
    names
        .iter()
        .find_map(|name| env(name).map(|value| (*name, value)))
}

/// ZCode 服务的 origin（Node `resolveRuntimeZCodeEndpointOrigin`）：CLI 登录的地址与模型请求的 `http-referer`（工作线
/// F64，model.md §2.4）共用这一个。`env` 同下。
pub(crate) fn zcode_origin(env: &impl Fn(&str) -> Option<String>) -> Result<String, String> {
    let test = is_test(env);
    Ok(
        match first(
            env,
            &[
                "ZCODE_BASE_URL",
                "ZCODE_ENDPOINT_ORIGIN",
                if test {
                    "ZCODE_TEST_BASE_URL"
                } else {
                    "ZCODE_PRODUCTION_BASE_URL"
                },
            ],
        ) {
            Some((name, value)) => origin(name, &value)?,
            None if test => ZCODE_TEST_ORIGIN.to_owned(),
            None => ZCODE_ORIGIN.to_owned(),
        },
    )
}

/// `env` 给出去掉首尾空白后非空的环境变量值；`version` 是签发项目令牌时上报的版本。
pub(crate) fn account_origins(
    env: impl Fn(&str) -> Option<String>,
    version: &str,
) -> Result<AccountOrigins, String> {
    let test = is_test(&env);
    let first = |names: &[&'static str]| first(&env, names);
    let zcode = zcode_origin(&env)?;
    let bigmodel = match first(&[
        "BIGMODEL_API_BASE_URL",
        if test {
            "BIGMODEL_TEST_API_BASE_URL"
        } else {
            "BIGMODEL_PRODUCTION_API_BASE_URL"
        },
    ]) {
        Some((name, value)) => origin(name, &value)?,
        None if test => BIGMODEL_TEST_ORIGIN.to_owned(),
        None => BIGMODEL_ORIGIN.to_owned(),
    };
    let zai = match first(&[
        "ZAI_BUSINESS_BASE_URL",
        if test {
            "ZAI_TEST_BUSINESS_BASE_URL"
        } else {
            "ZAI_PRODUCTION_BUSINESS_BASE_URL"
        },
    ]) {
        Some((name, value)) => origin(name, &value)?,
        None if test => ZAI_TEST_API.to_owned(),
        None => ZAI_API.to_owned(),
    };
    // 团队套餐的管理页只看环境（App 传进去的只有 `ZCODE_ENV`，F58）。
    let team_plans_origin = if test {
        BIGMODEL_TEST_ORIGIN
    } else {
        BIGMODEL_ORIGIN
    };
    Ok(AccountOrigins {
        zcode_api: format!("{zcode}/api/v1"),
        zai_api: zai,
        bigmodel_api: bigmodel,
        bigmodel_team_plans: format!("{team_plans_origin}/coding-plan/team/plans"),
        client_version: version.to_owned(),
    })
}

#[cfg(test)]
#[path = "origins_tests.rs"]
mod tests;
