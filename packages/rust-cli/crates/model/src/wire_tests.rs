//! 各协议的测试共用的请求。

use rust_cli_base::models;
use rust_cli_kernel_api::model::{
    ApiMessage, ModelRequest, ModelSelection, QuerySource, RequestOptions,
};

/// 没有 system、工具与思考的请求（选择是 GLM，`max_tokens` 32000）。
pub(crate) fn request(messages: Vec<ApiMessage>) -> ModelRequest {
    ModelRequest {
        selection: ModelSelection::new("anthropic".to_owned(), models::GLM_5_3.to_owned()),
        system: Vec::new(),
        messages,
        tools: Vec::new(),
        tool_choice: None,
        max_tokens: 32_000,
        thinking: None,
        temperature: None,
        betas: Vec::new(),
        source: QuerySource::MainThread,
        options: RequestOptions::default(),
    }
}
