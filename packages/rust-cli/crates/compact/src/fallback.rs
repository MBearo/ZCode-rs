//! “全部摘要”（docs/specs/rust-cli/compact.md §3.1）：最后的手段。全部摘要的切分、按候选选裁头的差、
//! 从头裁、检查摘要部分。

use rust_cli_kernel_api::message::Message;

use crate::engine::Failure;
use crate::{groups, tokens};

/// “全部摘要”的阶段。
pub(crate) struct AllPlan<'a> {
    pub summarize: Vec<&'a Message>,
    pub keep: Vec<&'a Message>,
    pub marker: bool,
    pub truncations: u32,
}

/// 转“全部摘要”（切分 + 按候选的 token 差裁头，spec §3.1）。
///
/// 缺陷修复（L25）：原来只看上一次按轮切分的差，反应式压缩不带触发它的错误给出的差，
/// 第一次就转全部摘要时整段对话发出去；初始差也应作为候选，按它裁掉开头。
pub(crate) fn summarize_all<'m>(
    messages: &[&'m Message],
    group_tokens: &[u64],
    last_round: Option<(Option<u64>, usize)>,
    initial_gap: Option<u64>,
    attempts: u32,
) -> Result<AllPlan<'m>, Failure> {
    // 失败原因：还没发过摘要请求为 too_few_groups，否则 exhausted。
    let failure = || {
        if attempts == 0 {
            Failure::NotEnough
        } else {
            Failure::Exhausted
        }
    };
    let (summarize, keep) = groups::split_all(messages);
    let kept_tokens = tokens::messages(keep.iter().copied());
    // 候选：上一次的差加上当时保留的各组；初始差。
    let mut candidates: Vec<u64> = Vec::new();
    if let Some((Some(gap), end)) = last_round {
        candidates.push(gap + group_tokens[end..].iter().sum::<u64>());
    }
    candidates.extend(initial_gap);
    // `Some(None)`：上一次的差解析不出，裁掉 20%。
    let hint: Option<Option<u64>> = match candidates.iter().max() {
        Some(max) => max
            .checked_sub(kept_tokens)
            .filter(|gap| *gap > 0)
            .map(Some),
        None => last_round.map(|_| None),
    };
    let mut plan = AllPlan {
        summarize,
        keep,
        marker: false,
        truncations: 0,
    };
    let check = match hint {
        // 不裁时只看摘要部分。
        None => plan.summarize.clone(),
        Some(gap) => {
            let (summarize, marker) =
                groups::truncate_head(&plan.summarize, gap).ok_or_else(failure)?;
            plan.summarize = summarize;
            plan.marker = marker;
            plan.truncations = 1;
            let mut check = plan.summarize.clone();
            check.extend(plan.keep.iter().copied());
            check
        }
    };
    if !groups::substantive(&check) {
        return Err(failure());
    }
    Ok(plan)
}
