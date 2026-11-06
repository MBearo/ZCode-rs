//! 回合失败的块（docs/puffin-tui.md §4.2「回合失败」「再试一次」；Puffin `shell/failures.rs` 的 `block`）：`✗` 与按原因写的
//! 句子、agent 的原话与 HTTP 状态与试过几次、^O 时错误的字段、下一步（`⏎ tries again.` 之类）。记号从 fg3 落定到 err，
//! 其余从地升起（`age` 是它到达以来的 tick）。措辞与画法都在 kit 的 `failure_block`。以前是一行红色的原文。
use rust_cli_tui_kit::app::widgets::failure_block::{self, Failure as KitFailure, Names};
use rust_cli_tui_kit::render::StyledLine;

use super::{Ctx, look};
use crate::ui::i18n::Lang;
use crate::ui::i18n_kit;
use crate::view::{Body, Failure, Row, ToolStatus};

/// 改文件的工具（Puffin `failures::EDITING`）。
const EDITING: [&str; 4] = ["Edit", "MultiEdit", "Write", "NotebookEdit"];

/// 句子里的名字（Puffin `failures::names`）：旧界面不留模型目录，按没有目录时的说法，provider 写它的 id，主机写
/// `the provider`；不知道 provider 时 `The provider`（中文是译文）。
fn names(f: &Failure, lang: Lang) -> Names {
    Names {
        provider: if f.provider.is_empty() {
            i18n_kit::the_provider(lang, true).to_owned()
        } else {
            f.provider.clone()
        },
        host: i18n_kit::the_provider(lang, false).to_owned(),
        signs_in: false,
        model: f.model.clone(),
    }
}

/// 失败的回合改过文件：失败块之前、它的消息之后有一个成功的编辑（Puffin `failures::edited`）。
fn edited(rows: &[Row], id: u64) -> bool {
    let at = rows
        .iter()
        .position(|row| row.id == id)
        .unwrap_or(rows.len());
    rows[..at]
        .iter()
        .rev()
        .take_while(|row| !matches!(row.body, Body::User { .. }))
        .any(|row| {
            row.tool().is_some_and(|tool| {
                tool.status == ToolStatus::Success && EDITING.contains(&tool.name.as_str())
            })
        })
}

/// kit 的失败（措辞与画法按它）。
pub(crate) fn kit_failure(f: &Failure) -> KitFailure<'_> {
    KitFailure {
        code: "",
        message: &f.message,
        reason: f.reason,
        http_status: f.http_status,
        provider_id: &f.provider,
        model_id: &f.model,
        error_phase: "",
        request_id: "",
        retryable: f.retryable,
        tries: f.tries,
        resend: !f.resend.is_empty(),
        at: f.at,
    }
}

/// 块说 `⏎ tries again.`（Puffin `failures::offers_again`）：⏎ 照原样再发这个回合的消息。
pub(crate) fn offers_again(f: &Failure) -> bool {
    failure_block::offers_again(&kit_failure(f))
}

/// 失败的块，`age` 是它到达以来的 tick（没有时已落定）；`rows` 是整个视图（看这个回合改没改过文件）；`back` 是上限说的
/// 回来的时刻那一行与它的年龄（usage_back.rs）。
pub(crate) fn lines(
    row: &Row,
    f: &Failure,
    rows: &[Row],
    age: Option<i64>,
    back: Option<(Vec<rust_cli_tui_kit::render::Span>, i64)>,
    ctx: &Ctx<'_>,
) -> Vec<StyledLine> {
    let it = kit_failure(f);
    let element = failure_block::block_said(
        &ctx.ink.theme,
        &it,
        &names(f, ctx.lang),
        edited(rows, row.id),
        ctx.expanded,
        age.unwrap_or(rust_cli_tui_kit::markdown::render::SETTLED),
        back,
        0,
        i18n_kit::failures(ctx.lang),
    );
    look::lay(&element, ctx.width)
}
