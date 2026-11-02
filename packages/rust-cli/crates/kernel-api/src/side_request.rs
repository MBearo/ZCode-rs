//! 旁路请求（审查 E5 / T11 / C13，docs/specs/rust-cli/model.md §2.4、§5.2）：不经主循环、直接经模型端口发的请求——
//! 会话标题、`/rename` 起名、prompt hook、WebFetch 摘要、WebSearch。
//!
//! 来源是一个封闭的枚举 [`SideSource`]：线上名字只在 [`SideSource::as_wire`] 写一处，过载重试、安静与 diagnostics
//! 三条规则是枚举上的穷举 `match`（加一种来源时三处都要当场决定）。以前来源是 `Side(String)`，调用方写字面量，
//! model 与 query 各按一张名单（11 个与 4 个名字，多数没有生产者）改变行为。

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::ids::{AgentId, RunId, SessionId};
use crate::message::{ContentBlock, StopReason, Usage};
use crate::model::{
    ApiContent, ApiMessage, ApiRole, BlockDelta, ModelError, ModelEvent, ModelRequest,
    ModelSelection, QuerySource, RequestOptions, SystemBlock,
};

/// 旁路请求的来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SideSource {
    /// 自动的会话标题（titles.md §3）。
    SessionTitle,
    /// `/rename` 起名（titles.md §8.4）。
    Rename,
    /// prompt hook 的评估（hooks.md §7）。
    HookPrompt,
    /// WebFetch 的摘要（tools-more.md §4）。
    WebFetch,
    /// WebSearch（tools-more.md §5）。
    WebSearch,
}

impl SideSource {
    /// 全部来源（`from_wire` 按它查；测试核对它列全了）。
    pub const ALL: [Self; 5] = [
        Self::SessionTitle,
        Self::Rename,
        Self::HookPrompt,
        Self::WebFetch,
        Self::WebSearch,
    ];

    /// 本工程的来源名（`QuerySource` 序列化后的 `name`；用量表与请求头另经 [`Self::node_name`]）。
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::SessionTitle => "generate_session_title",
            Self::Rename => "rename_generate_name",
            Self::HookPrompt => "hook_prompt",
            Self::WebFetch => "web_fetch_apply",
            Self::WebSearch => "web_search_tool",
        }
    }

    pub fn from_wire(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|source| source.as_wire() == name)
    }

    /// 写给 Node 的用途名（用量表的 `query_source` 与请求头 `x-zcode-query-source`，工作线 F71）：Node 有对应用途的用
    /// Node 的名字，其余与 [`Self::as_wire`] 相同。
    pub const fn node_name(self) -> &'static str {
        match self {
            Self::SessionTitle => "session_title",
            Self::WebFetch => "web_fetch_processing",
            Self::Rename | Self::HookPrompt | Self::WebSearch => self.as_wire(),
        }
    }

    /// 用户在等结果：遇过载照主循环重试（model.md §5.2 第 1 条）；其余旁路请求过载时立即失败。
    pub const fn foreground(self) -> bool {
        match self {
            Self::HookPrompt | Self::WebFetch | Self::WebSearch => true,
            Self::SessionTitle | Self::Rename => false,
        }
    }

    /// 安静的来源：跑主循环时不做 thinking-only 的提示，也不做自动与反应式压缩（query.md §3、compact.md §1）。
    pub const fn quiet(self) -> bool {
        match self {
            Self::HookPrompt => true,
            Self::SessionTitle | Self::Rename | Self::WebFetch | Self::WebSearch => false,
        }
    }

    /// 自己跑一个代理循环的来源（对照实现的 agent hook）：一方端点写 `diagnostics`（model.md §3.1）。现在没有这种来源。
    pub const fn agentic(self) -> bool {
        match self {
            Self::SessionTitle
            | Self::Rename
            | Self::HookPrompt
            | Self::WebFetch
            | Self::WebSearch => false,
        }
    }
}

// 序列化成来源名（`QuerySource` 的 `{"kind": "side", "name": …}`，以前是字符串，字节不变）。
impl Serialize for SideSource {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for SideSource {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::from_wire(&name)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown side source {name}")))
    }
}

/// 发旁路请求的一方：会话（`metadata.user_id` 的 `session_id`，model.md §3.3）、回合（`x-query-id`）与子代理
/// （`x-zcode-session-type`，model.md §2.4）。没有回合的请求（`/rename` 起名）`run` 为 None。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requester {
    pub session: SessionId,
    pub run: Option<RunId>,
    pub agent: Option<AgentId>,
}

impl ApiMessage {
    /// 一条只有文字、没有缓存断点的 user 消息。
    pub fn user_text(text: String) -> Self {
        Self {
            role: ApiRole::User,
            content: vec![ApiContent {
                block: ContentBlock::Text { text },
                cache_control: None,
            }],
            output_config: None,
        }
    }
}

impl ModelRequest {
    /// 一个旁路请求：没有工具、`tool_choice`、thinking、temperature 与 beta；发请求的一方写进 `options`。其余选项
    /// （effort、structured output、服务端工具、交给 Provider 补丁的上限）由调用方在 `options` 上补。
    pub fn side(
        selection: ModelSelection,
        source: SideSource,
        system: Vec<SystemBlock>,
        messages: Vec<ApiMessage>,
        max_tokens: u32,
        requester: Requester,
    ) -> Self {
        Self {
            selection,
            system,
            messages,
            tools: Vec::new(),
            tool_choice: None,
            max_tokens,
            thinking: None,
            temperature: None,
            betas: Vec::new(),
            source: QuerySource::Side(source),
            options: RequestOptions {
                session_id: Some(requester.session),
                run: requester.run,
                agent: requester.agent,
                ..RequestOptions::default()
            },
        }
    }
}

/// 读一个旁路请求的流得到的回复：块按出现的顺序，文字增量接在同一序号的文字块上（只认文字的增量）；用量是
/// `message_start` 的，之后由 `message_delta` 合并好的累计用量整体替换（同主循环）；停止原因取最后给出的。
/// 模型适配器保证块的开始在它的增量之前。
#[derive(Debug, Default)]
pub struct SideReply {
    blocks: Vec<(usize, ContentBlock)>,
    pub usage: Usage,
    pub stop_reason: Option<StopReason>,
}

impl SideReply {
    /// 读完整个流；遇到第一个错误就返回它。
    pub async fn collect(
        mut stream: BoxStream<'static, Result<ModelEvent, ModelError>>,
    ) -> Result<Self, ModelError> {
        let mut reply = Self::default();
        while let Some(event) = stream.next().await {
            reply.push(event?);
        }
        Ok(reply)
    }

    pub fn push(&mut self, event: ModelEvent) {
        match event {
            ModelEvent::MessageStart { usage, .. } => self.usage = usage,
            ModelEvent::BlockStart { index, block } => self.blocks.push((index, block)),
            ModelEvent::BlockDelta {
                index,
                delta: BlockDelta::Text(more),
            } => {
                if let Some((_, ContentBlock::Text { text })) =
                    self.blocks.iter_mut().find(|(at, _)| *at == index)
                {
                    text.push_str(&more);
                }
            }
            ModelEvent::MessageDelta { stop_reason, usage } => {
                self.stop_reason = stop_reason.or(self.stop_reason);
                self.usage = usage;
            }
            ModelEvent::BlockDelta {
                delta: BlockDelta::Thinking(_) | BlockDelta::Signature(_) | BlockDelta::InputJson(_),
                ..
            }
            | ModelEvent::BlockStop { .. }
            | ModelEvent::MessageStop => {}
        }
    }

    /// 全部文字块接起来。
    pub fn text(&self) -> String {
        self.blocks
            .iter()
            .filter_map(|(_, block)| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// 第一个块的文字（不是文字块时为空）；一个块都没有时 None。
    pub fn first_text(&self) -> Option<&str> {
        self.blocks.first().map(|(_, block)| match block {
            ContentBlock::Text { text } => text.as_str(),
            _ => "",
        })
    }

    /// tool_use 块的个数。
    pub fn tool_calls(&self) -> u32 {
        let count = self
            .blocks
            .iter()
            .filter(|(_, block)| matches!(block, ContentBlock::ToolUse { .. }))
            .count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    /// 块，按出现的顺序。
    pub fn into_blocks(self) -> Vec<ContentBlock> {
        self.blocks.into_iter().map(|(_, block)| block).collect()
    }
}

#[cfg(test)]
#[path = "side_request_tests.rs"]
mod tests;
