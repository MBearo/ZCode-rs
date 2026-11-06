//! 压缩核心（docs/specs/rust-cli/compact.md §3）：分组、保留尾段、PTL 阶梯、摘要消息与边界、
//! 压缩后附件、hook 接入点。三种触发共用。

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};

use rust_cli_kernel_api::history;
use rust_cli_kernel_api::message::{
    CompactBoundary, ContentBlock, Message, PreservedSegment, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::QuerySource;
use rust_cli_kernel_api::orchestration::{
    CompactProgress, CompactRequest, CompactResult, ContextRequest, QueryRequest,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::tool_search;
use rust_cli_prompts::compact as texts;
use tokio_util::sync::CancellationToken;

use crate::fallback::{self, AllPlan};
use crate::groups::{self, flat};
use crate::hooks;
use crate::request::{self, Attempt};
use crate::restore::Restore;
use crate::summary::{self, SummaryShape};
use crate::{CompactPorts, tokens};

/// 有初始差时，组数多于它才按差多保留几组。
const SEED_MIN_GROUPS: usize = 3;

/// 全部摘要时最多从头裁几次。
const MAX_HEAD_TRUNCATIONS: u32 = 3;

/// 压缩失败的原因；`Display` 是给用户的原文。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Failure {
    #[error("No messages to compact")]
    NoMessages,
    #[error("Not enough messages to compact.")]
    NotEnough,
    #[error("Compaction blocked by PreCompact hook: {0}")]
    Blocked(String),
    #[error("Compaction failed · conversation could not be reduced below the context limit")]
    Exhausted,
    #[error("Compaction canceled.")]
    Aborted,
    #[error("Error during compaction: {0}")]
    Error(String),
}

/// 一次摘要请求的输入。
struct Split<'a> {
    summarize: Vec<&'a Message>,
    keep: Vec<&'a Message>,
    /// 开头被裁掉过；以 assistant 开头时要补一条截断说明。
    truncated: Option<bool>,
}

/// 末尾有多少组完全在原始历史的最后一个边界之后：只有它们能作为保留段（保留段在存储里是一段
/// 连续的 `head..=tail`）。
fn preservable(history: &[Message], groups: &[Vec<&Message>]) -> usize {
    let fresh: HashSet<&str> = match history::last_boundary(history) {
        Some((index, _)) => history[index + 1..]
            .iter()
            .map(|m| m.id().as_str())
            .collect(),
        None => history.iter().map(|m| m.id().as_str()).collect(),
    };
    groups
        .iter()
        .rev()
        .take_while(|group| group.iter().all(|m| fresh.contains(m.id().as_str())))
        .count()
}

/// 压缩的进度（spec §4.1）：第一次摘要请求之前说开始；说过开始之后失败（不是中止）时说失败。没发摘要请求就放弃的
/// （分组不够、不许全部摘要）两样都不说。
struct Announce<'a> {
    progress: Option<&'a dyn CompactProgress>,
    trigger: rust_cli_kernel_api::message::CompactTrigger,
    started: AtomicBool,
}

impl Announce<'_> {
    async fn start(&self) {
        if let Some(progress) = self.progress
            && !self.started.swap(true, Ordering::Relaxed)
        {
            progress.started(self.trigger).await;
        }
    }

    async fn fail(&self, failure: &Failure) {
        if let Some(progress) = self.progress
            && self.started.load(Ordering::Relaxed)
            && *failure != Failure::Aborted
        {
            progress.failed(self.trigger, failure.to_string()).await;
        }
    }
}

pub(crate) struct Engine<'a> {
    pub ports: &'a CompactPorts,
}

impl Engine<'_> {
    /// `fallback`：摘要部分没有 assistant 时许不许“全部摘要”（spec §3.1）。`progress`：第一次摘要请求之前说开始，
    /// 之后失败（不是中止）时说失败（spec §4.1）；调用方已经说过开始时为 None。
    pub(crate) async fn run(
        &self,
        request: &CompactRequest,
        fallback: bool,
        cancel: &CancellationToken,
        progress: Option<&dyn CompactProgress>,
    ) -> Result<CompactResult, Failure> {
        let active = history::active(&request.messages);
        let refs: Vec<&Message> = active.iter().map(AsRef::as_ref).collect();
        let pre_tokens = rust_cli_kernel_api::compaction::context_tokens(&refs);
        let messages: Vec<&Message> = refs
            .iter()
            .copied()
            .filter(|m| !matches!(m, Message::System(_)))
            .collect();
        if messages.is_empty() {
            return Err(Failure::NoMessages);
        }
        // 压缩前已加载的工具（引用过的，加上活动历史开头那条边界带的）记在新边界上，压缩之后照样算已加载
        // （docs/specs/rust-cli/compact.md §3.5）。以前不记，压缩之后已加载的延迟工具从 tools 里消失。
        let discovered: Vec<String> = tool_search::loaded(refs.iter().copied())
            .into_iter()
            .collect();
        let instructions = hooks::pre_compact(self.ports.hooks.as_ref(), request, cancel).await?;
        let groups = groups::group(&messages);
        if groups.len() < groups::MIN_GROUPS {
            return Err(Failure::NotEnough);
        }
        let announce = Announce {
            progress,
            trigger: request.trigger,
            started: AtomicBool::new(false),
        };
        let summarized = async {
            let (raw, split) = self
                .ladder(
                    request,
                    &messages,
                    &groups,
                    instructions.as_deref(),
                    fallback,
                    cancel,
                    &announce,
                )
                .await?;
            self.finalize(request, pre_tokens, raw, split, cancel).await
        }
        .await;
        match summarized {
            Ok(mut result) => {
                result.boundary.discovered_tools = discovered;
                Ok(result)
            }
            Err(failure) => {
                announce.fail(&failure).await;
                Err(failure)
            }
        }
    }

    async fn attempt(
        &self,
        request: &CompactRequest,
        split: &Split<'_>,
        instructions: Option<&str>,
        cancel: &CancellationToken,
    ) -> Attempt {
        let meta = |text: &str, source: &str| {
            Message::User(UserMessage {
                id: self.ports.ids.message(),
                content: vec![ContentBlock::Text {
                    text: text.to_owned(),
                }],
                origin: UserOrigin::Meta {
                    source: source.to_owned(),
                },
            })
        };
        // 要压缩的消息之外：截断说明与压缩请求。
        const EXTRA: usize = 2;
        let mut messages = Vec::with_capacity(split.summarize.len() + EXTRA);
        if split.truncated == Some(true) {
            messages.push(meta(texts::truncated_head(), "compact_truncated"));
        }
        messages.extend(split.summarize.iter().map(|m| (*m).clone()));
        // 摘要提示是一条普通 user 消息。
        messages.push(Message::User(UserMessage {
            id: self.ports.ids.message(),
            content: vec![ContentBlock::Text {
                text: summary::prompt(instructions),
            }],
            origin: UserOrigin::Human,
        }));
        let query = QueryRequest {
            agent: request.agent.clone(),
            max_turns: Some(1),
            ..QueryRequest::new(
                request.session.clone(),
                self.ports.ids.run(),
                request.workspace.clone(),
                messages,
                request.selection.clone(),
                request.mode.unwrap_or(PermissionMode::Default),
                rust_cli_kernel_api::model::QuerySource::Compact,
            )
        };
        request::run(self.ports.runner.as_ref(), query, cancel).await
    }

    /// 保留尾段的阶梯（spec §3.1）：返回摘要原文与这次的切分。
    #[allow(clippy::too_many_arguments)]
    async fn ladder<'m>(
        &self,
        request: &CompactRequest,
        messages: &[&'m Message],
        groups: &[Vec<&'m Message>],
        instructions: Option<&str>,
        fallback: bool,
        cancel: &CancellationToken,
        announce: &Announce<'_>,
    ) -> Result<(String, Split<'m>), Failure> {
        let total = groups.len();
        let preservable = preservable(&request.messages, groups);
        let group_tokens: Vec<u64> = groups
            .iter()
            .map(|g| tokens::messages(g.iter().copied()))
            .collect();
        let mut keep = 1;
        // 播种（有初始差且组数 > 3）：差减去最后一组仍 > 0 时，按剩下的差多保留几组。
        if let Some(gap) = request.initial_token_gap
            && total > SEED_MIN_GROUPS
        {
            let remaining = gap.saturating_sub(group_tokens[total - 1]);
            if remaining > 0 {
                keep += groups::preserve_step(&group_tokens, total - 1, Some(remaining));
            }
        }
        let mut all: Option<AllPlan<'m>> = None;
        // 上一次按轮切分时的 token 差与摘要部分的终点。
        let mut last_round: Option<(Option<u64>, usize)> = None;
        // 发过几次摘要请求。
        let mut attempts = 0;
        loop {
            if cancel.is_cancelled() {
                return Err(Failure::Aborted);
            }
            let split = match &all {
                Some(plan) => Split {
                    summarize: plan.summarize.clone(),
                    keep: plan.keep.clone(),
                    truncated: (plan.truncations > 0).then_some(plan.marker),
                },
                None => {
                    let end = total.saturating_sub(keep);
                    let summarize = flat(&groups[..end]);
                    let has_assistant = groups::has_assistant(&summarize);
                    if keep <= preservable && has_assistant {
                        Split {
                            summarize,
                            keep: flat(&groups[end..]),
                            truncated: None,
                        }
                    } else if !has_assistant && !fallback {
                        // 缺陷修复（L25）：阈值触发的自动压缩原来照常走“全部摘要”、发摘要请求；
                        // 没到阻塞线时不许“全部摘要”，摘要部分没有 assistant 就放弃（原因按发过几次
                        // 摘要请求定）。
                        return Err(if attempts == 0 {
                            Failure::NotEnough
                        } else {
                            Failure::Exhausted
                        });
                    } else {
                        all = Some(fallback::summarize_all(
                            messages,
                            &group_tokens,
                            last_round,
                            request.initial_token_gap,
                            attempts,
                        )?);
                        continue;
                    }
                }
            };
            // 缺陷修复（spec §4.1）：以前自动压缩的开始事件由主循环在摘要完成之后才发，摘要请求期间前端只看到 `Working`。
            announce.start().await;
            attempts += 1;
            match self.attempt(request, &split, instructions, cancel).await {
                Attempt::Summary { text, .. } => return Ok((text, split)),
                Attempt::Aborted => return Err(Failure::Aborted),
                Attempt::Failed { detail } => return Err(Failure::Error(detail)),
                Attempt::PromptTooLong { gap } => match &mut all {
                    Some(plan) => {
                        if plan.truncations >= MAX_HEAD_TRUNCATIONS {
                            return Err(Failure::Exhausted);
                        }
                        let (summarize, marker) = groups::truncate_head(&plan.summarize, gap)
                            .ok_or(Failure::Exhausted)?;
                        let mut check = summarize.clone();
                        check.extend(plan.keep.iter().copied());
                        if !groups::substantive(&check) {
                            return Err(Failure::Exhausted);
                        }
                        plan.summarize = summarize;
                        plan.marker = marker;
                        plan.truncations += 1;
                    }
                    None => {
                        let end = total - keep;
                        last_round = Some((gap, end));
                        keep += groups::preserve_step(&group_tokens, end, gap);
                    }
                },
            }
        }
    }

    /// 压缩附件里的计划模式、调用过的 skill 与清单增量（spec §3.4），排在文件之后；只在主线程。
    /// 生成失败时不带（吞掉错误），下一步由 context 照常宣布。
    async fn listings(&self, request: &CompactRequest, kept: &[&Message]) -> Vec<Message> {
        if request.agent.is_some() {
            return Vec::new();
        }
        let context = ContextRequest {
            tool_names: self
                .ports
                .tools
                .tools()
                .into_iter()
                .filter(|tool| tool.is_enabled())
                .map(|tool| tool.name().to_owned())
                .collect(),
            ..ContextRequest::new(
                request.session.clone(),
                request.workspace.clone(),
                QuerySource::MainThread,
                request.mode.unwrap_or(PermissionMode::Default),
                request.selection.clone(),
            )
        };
        let kept: Vec<Message> = kept.iter().map(|message| (*message).clone()).collect();
        // 压缩前的全部历史：调用过的 skill 从它取（spec §3.4）。
        self.ports
            .context
            .post_compact_with_history(context, &request.messages, &kept)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(Message::Attachment)
            .collect()
    }

    async fn finalize(
        &self,
        request: &CompactRequest,
        pre_tokens: u64,
        raw: String,
        split: Split<'_>,
        cancel: &CancellationToken,
    ) -> Result<CompactResult, Failure> {
        let ids = self.ports.ids.as_ref();
        let boundary_id = ids.message();
        let summary_id = ids.message();
        let shape = SummaryShape {
            head_truncated: split.truncated.is_some(),
            kept_tail: !split.keep.is_empty(),
        };
        let summary = UserMessage {
            id: summary_id.clone(),
            content: vec![ContentBlock::Text {
                text: summary::message_text(&raw, shape),
            }],
            origin: UserOrigin::CompactSummary,
        };
        let preserved = match (split.keep.first(), split.keep.last()) {
            (Some(head), Some(tail)) => Some(PreservedSegment {
                head: head.id().clone(),
                anchor: summary_id.clone(),
                tail: tail.id().clone(),
            }),
            _ => None,
        };
        let restore = Restore {
            tools: self.ports.tools.as_ref(),
            machine: self.ports.machines.primary_machine(),
            controller: self.ports.machines.controller(),
            artifacts: self.ports.artifacts.clone(),
            ids,
            operations: self.ports.operations.clone(),
        };
        // 缺陷修复（L25，用户同意按缺陷修）：原来不清空 readFileState，从历史里选文件重读，Read
        // 判断“读过且未变”，压缩后的文件附件是 `Wasted call…`。应在这里先复制、再清空，用复制品选文件，
        // 重读时状态已空。`clear` 一次做完复制与清空。
        let recent = self.ports.files.clear(&request.session);
        let mut restored: Vec<Message> = restore
            .files(request, &recent, &split.keep, cancel)
            .await
            .into_iter()
            .map(Message::Attachment)
            .collect();
        restored.extend(self.listings(request, &split.keep).await);
        // SessionStart（`compact`）的消息排在附件之后，三种触发都跑。
        restored.extend(hooks::session_start(self.ports.hooks.as_ref(), request, cancel).await);
        hooks::post_compact(self.ports.hooks.as_ref(), request, &raw, cancel).await;
        Ok(CompactResult {
            boundary: CompactBoundary {
                id: boundary_id,
                trigger: request.trigger,
                pre_tokens,
                summary: summary_id,
                preserved,
                discovered_tools: Vec::new(),
            },
            summary,
            restored,
        })
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "engine_gap_tests.rs"]
mod gap_tests;

#[cfg(test)]
#[path = "carried_tools_tests.rs"]
mod carried_tools_tests;
