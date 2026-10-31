//! platform 里跨模块的常量（docs/specs/rust-cli/constants.md §2）：按含义分段。

// ---- 平台接口的业务成功码（响应体里的 `code`，不是 HTTP 状态码） ----

/// 业务成功码 200（pricing 的信封只认它）。
pub(crate) const BUSINESS_CODE_OK: f64 = 200.0;
/// 业务成功码的全集：0 与 200（customerInfo 的信封、`successful_data`、管理接口与签发）。
pub(crate) const BUSINESS_SUCCESS_CODES: [f64; 2] = [0.0, BUSINESS_CODE_OK];
