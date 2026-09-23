# Rust 模型协议

状态：通过本地原生/App 验收，见 [交付记录](../reports/rust-model-protocols-2026-09-22.md)。承接上下文包；权限仍仅 yolo，默认 runtime 仍为 TS。

## 行为与边界

- 显式原生配置新增 apiType：openai-chat-completions（兼容缺省）、openai-responses、anthropic-messages。baseUrl 为 API 根：Chat/Responses 分别追加 chat/completions、responses；Anthropic 与 TS 一致，缺少 /v1 时先追加 /v1，再追加 /messages（保留网关路径）。Anthropic 同时发送 x-api-key 与 Bearer Authorization，并发送 anthropic-version；不根据模型名称猜协议。
- HttpModel 仍唯一拥有 HTTP client、一次请求编码及重试/取消策略。所有协议复用 SSE decoder、TextBuffer 和现有模型错误类型；不能在 app/domain 引入网络或 provider 实现。
- canonical assistant / tool 仍由 Session actor 提交；供应商事件映射为同一 Text、ModelDone、ToolDone，App 无协议版本变化。正文、推理首段和后续合并规则保持一致。
- Responses 使用 store=false、完整 input item history；工具定义使用平铺 function，工具结果关联 call_id。保留 reasoning items 的 encrypted_content 与 summary，下一次请求原样回传，不能把加密元数据展示为正文。
- Anthropic system 与 messages 分开；assistant 的 text/thinking/redacted_thinking/tool_use 和 user tool_result 依原协议传递，保留签名。工具参数独立增量汇编，块关闭后校验 JSON 对象；只有 message_stop + 合法 stop_reason 才能交付工具。
- canonical 的供应商元数据仅由对应 serializer 消费，不泄漏到 Chat Completions body 或 App。工具失败通过 Anthropic is_error 映射。
- 所有协议验证工具 id/name/参数与结束状态；缺失终止事件不得执行工具。可见正文/推理后断流不透明重放。未知无业务含义事件忽略；未支持的工具类型明确失败。
- 显式 reasoningParameters 按协议校验：Chat 使用 reasoning_effort/thinking/enable_thinking，Responses 使用 reasoning 对象或 reasoning_effort 转 effort（二者互斥），Anthropic 使用 thinking。maxOutputTokens 分别映射 max_tokens/max_output_tokens。
- App Registry、动态模型选择与请求期账户鉴权仍是后续步骤；不因支持新 HTTP 协议而虚报账号配置能力。

```mermaid
sequenceDiagram
    participant Owner as Session actor
    participant Loop as Agent loop
    participant HTTP as HttpModel
    participant Adapter as Protocol assembly
    Loop->>HTTP: canonical messages + tools
    HTTP->>HTTP: encode once / retry policy
    HTTP->>Adapter: bounded SSE event
    Adapter-->>Owner: ordered text / reasoning
    Adapter-->>Loop: validated ModelOutput
    Loop->>Owner: ModelDone + commit receipt
    Owner-->>Loop: committed
    Loop->>Loop: execute tools / next request
```

## 验收

三个协议的真实 Rust 子进程请求路径、headers、正文/推理、分片工具参数、工具结果回传、冷恢复元数据、断流/终态错误、重试字节复用、取消与 compact。不得请求真实模型或读取个人账号；使用本地 HTTP fixture 和当前 App schema。

协议依据：当前 TS adapters/model 与公开协议：[OpenAI function calling](https://developers.openai.com/api/docs/guides/function-calling)、[Responses migration](https://developers.openai.com/api/docs/guides/migrate-to-responses)、[Anthropic streaming](https://platform.claude.com/docs/en/build-with-claude/streaming)。正文/工具核心不等于完整供应商 hosted tools、websocket、batch 或多媒体支持。

性能验收：保留固定每轮文本片段与轮次数，为 Responses/Anthropic 只替换 SSE 协议封装；两端 contextWindow 均为 256000，避免摘要改变负载。release 同机串行交替各五次，比较 Chat 新旧版本以及同产物三协议，记录启动/首段/总耗时/RPC p95/RSS/存储；协议传输字节数不同，结果只代表本地 fixture 的 adapter 开销。
