//! 按请求方案改写 Anthropic 请求（docs/specs/rust-cli/model.md §3.2，修复线 F23，用户决定 A6）。
//!
//! query 与 context 只产生一种形状（第一方形状，卡片模型带卡片字段）。一方原样发出；“其他 Anthropic 兼容”
//! 在编码前改成第三方 system 形状、去掉 `scope`，并去掉一方专有项，贴近 Node 在同一端点上的请求。

use rust_cli_kernel_api::model::{CacheControl, ModelRequest, SystemBlock};

use crate::profiles::{Profile, SystemShape};

/// 编码前按方案改写请求。
pub(crate) fn apply(request: &mut ModelRequest, profile: &Profile) {
    if profile.system == SystemShape::ThirdParty {
        third_party_system(&mut request.system);
        drop_cache_scope(request);
    }
    if !profile.first_party_extras {
        drop_first_party_extras(request);
    }
}

fn ephemeral() -> Option<CacheControl> {
    Some(CacheControl {
        kind: "ephemeral".to_owned(),
        ttl: None,
        scope: None,
    })
}

/// 不走全局缓存时的缺省分段：有
/// `scope: global` 的块时，它之前的块（身份句）各加 `{ephemeral}`，它与之后的块用空行连接成一块 `{ephemeral}`。
/// 没有这种块（子代理、旁路请求，两种形状相同）时不变。
pub(crate) fn third_party_system(system: &mut Vec<SystemBlock>) {
    let global = |block: &SystemBlock| {
        block
            .cache_control
            .as_ref()
            .is_some_and(|cache| cache.scope.as_deref() == Some("global"))
    };
    let Some(at) = system.iter().position(global) else {
        return;
    };
    let rest = system.split_off(at);
    for block in system.iter_mut() {
        if block.cache_control.is_none() {
            block.cache_control = ephemeral();
        }
    }
    let text = rest
        .into_iter()
        .map(|block| block.text)
        .collect::<Vec<_>>()
        .join("\n\n");
    system.push(SystemBlock {
        text,
        cache_control: ephemeral(),
    });
}

/// 去掉请求里全部缓存断点的 `scope`（system、tools、消息块），其余缓存字段不变。
///
/// 修 bug（修复线 F23 救急）：以前所有 Anthropic 协议端点都写静态段的 `scope: global`，经网关转到 Bedrock 时整个
/// 请求被拒（`system.1.cache_control.ephemeral.scope: Extra inputs are not permitted`）。只应在第一方 base URL
/// 上写 `scope`；这里对第三方形状在编码前去掉，`prompt-caching-scope` beta 随之不发。
pub(crate) fn drop_cache_scope(request: &mut ModelRequest) {
    let system = request
        .system
        .iter_mut()
        .map(|block| &mut block.cache_control);
    let tools = request.tools.iter_mut().map(|tool| &mut tool.cache_control);
    let messages = request
        .messages
        .iter_mut()
        .flat_map(|message| message.content.iter_mut())
        .map(|content| &mut content.cache_control);
    for cache in system.chain(tools).chain(messages).flatten() {
        cache.scope = None;
    }
}

/// 去掉 query 按模型卡片写的卡片字段：`thinking`、`effort`、`context_management`、消息级的每轮 effort；调用方
/// 显式给的 beta 也不发。推理字段由配置的 option map 补丁写（model.md §9），`diagnostics` 由编码器按方案不写。
fn drop_first_party_extras(request: &mut ModelRequest) {
    request.thinking = None;
    request.options.effort = None;
    request.options.context_edits.clear();
    request.betas.clear();
    for message in &mut request.messages {
        message.output_config = None;
    }
}

#[cfg(test)]
#[path = "anthropic_shape_tests.rs"]
mod tests;

#[cfg(test)]
#[cfg(feature = "parity")]
#[rustfmt::skip]
#[path = "anthropic_shape_parity_tests.rs"]
mod parity_tests;
