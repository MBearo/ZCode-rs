//! 端点端口的缺省实现与请求选项（工作线 F95，docs/specs/rust-cli/model.md §9）。

use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::json;

use super::*;
use crate::model::RequestOptions;

/// 只实现 `endpoint` 的端点（固定端点、测试替身的写法），记下被调用的选择。
struct Recording(Mutex<Vec<String>>);

#[async_trait]
impl ModelEndpoints for Recording {
    async fn endpoint(
        &self,
        selection: &ModelSelection,
        _rejected: Option<&ModelEndpoint>,
    ) -> Result<ModelEndpoint, ModelError> {
        self.0.lock().unwrap().push(selection.model.clone());
        Ok(ModelEndpoint {
            protocol: WireProtocol::AnthropicMessages,
            base_url: "https://api.example.com".to_owned(),
            model: selection.model.clone(),
            credential: Credential::None,
            headers: Vec::new(),
            body_patches: vec![json!({"max_tokens": 128_000})],
            mid_conversation_system: true,
            supports_image: true,
            supports_pdf: true,
            supports_video: false,
            supports_audio: false,
            renewable: false,
        })
    }
}

/// 缺省实现忽略上限，调 `endpoint`：没有补丁的端点不用改。
#[tokio::test]
async fn the_default_output_limit_endpoint_is_the_plain_endpoint() {
    let endpoints = Recording(Mutex::new(Vec::new()));
    let selection = ModelSelection {
        provider: "p".to_owned(),
        model: "m".to_owned(),
        reasoning: None,
        connection: None,
    };
    let limited = endpoints
        .endpoint_with_output_limit(&selection, None, Some(4096))
        .await
        .unwrap();
    assert_eq!(limited, endpoints.endpoint(&selection, None).await.unwrap());
    assert_eq!(*endpoints.0.lock().unwrap(), ["m", "m"]);
}

/// 交给补丁的上限：没有时不写出，有时原样往返。
#[test]
fn the_provider_output_limit_is_optional_on_the_wire() {
    assert_eq!(
        serde_json::to_value(RequestOptions::default()).unwrap(),
        json!({})
    );
    let options = RequestOptions {
        provider_max_output_tokens: Some(4096),
        ..RequestOptions::default()
    };
    let value = serde_json::to_value(&options).unwrap();
    assert_eq!(value, json!({"provider_max_output_tokens": 4096}));
    assert_eq!(
        serde_json::from_value::<RequestOptions>(value).unwrap(),
        options
    );
}
