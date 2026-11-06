//! Models 的子页：你的 provider、新 provider 与一个模型（照抄 Puffin `shell/settings/pages.rs` 的 `model_item`、`provider`、
//! `new_provider`、`provider_form` 与 `model`）。从 `sections.rs` 分出（文件行数的上限）。
//!
//! 页从读 Models 段时读到的 provider 与家族建（runner 按模型目录分好，同 Puffin host 的 `settings.rs` 里的
//! `models`）：目录里的 provider 没有格式、地址与密钥（空的），它的模型都开着、都是内置的；没有模板。改 provider、开关模型、换计划、添加、
//! 删除与测试连接两边的 host 都还没有（tui-kit.md §4.5）：同 Puffin 照样经 runner 写与测，runner 回答这一版还没
//! 有，说的话与回答到的那一帧都同 Puffin。
use std::sync::Arc;

use rust_cli_tui_kit::app::widgets::settings::{self as kit, Applies, Ink, Opt, Part};

use super::page::{Act, Change, Ctx, ItemRow, Page, ProviderEdit, Row, TextRow, View, Write};
use super::tokens::grouped;
use crate::services::FamilyModel;

/// 写到下一个会话生效的改动（Puffin `next`）。
pub(super) fn next(change: Change) -> Act {
    Act::Write(Write::new(change, Applies::Next))
}

/// provider 的一项改动。
pub(super) fn edit(provider: &str, edit: ProviderEdit) -> Act {
    next(Change::Provider {
        provider: provider.to_owned(),
        edit,
    })
}

/// API 格式：值、名字与它的路径（Puffin `FORMATS`）。
const FORMATS: [(&str, &str, &str); 3] = [
    (
        "openai-chat-completions",
        "Chat completions",
        "/v1/chat/completions",
    ),
    ("openai-responses", "Responses", "/responses"),
    ("anthropic-messages", "Anthropic messages", "/v1/messages"),
];

/// 模型的行（Puffin `model_item` 与 `model_status`）：⏎ 进它的页，空格开关它。
pub(super) fn model_item(provider: &str, m: &FamilyModel, desc: &str) -> Row {
    let status = if m.enabled {
        m.context_window
            .map(|n| format!("{} context", kit::tokens_short(n)))
            .unwrap_or_default()
    } else {
        "off".to_owned()
    };
    Row::item(ItemRow {
        key: format!("model:{provider}:{}", m.id),
        name: m.id.clone(),
        toggle: Some(m.enabled),
        status: vec![Part::new(status, Ink::Fg3)],
        desc: desc.to_owned(),
        open: Some(Act::Push(Page::Model {
            provider: provider.to_owned(),
            model: m.id.clone(),
        })),
        flip: Some(next(Change::Model {
            provider: provider.to_owned(),
            model: m.id.clone(),
            enabled: !m.enabled,
        })),
        ..ItemRow::default()
    })
}

/// 你的一个 provider：名字、格式、地址、密钥、开关与它的模型（Puffin `provider`）。
pub(super) fn provider(id: &str, ctx: &Ctx<'_>) -> View {
    let Some(p) = ctx
        .models()
        .and_then(|m| m.providers.iter().find(|p| p.id == id))
    else {
        return View::loading("Provider");
    };
    let with = |f: fn(String) -> ProviderEdit| {
        let id = id.to_owned();
        move |v: &str| edit(&id, f(v.to_owned()))
    };
    let mut base = TextRow::new(
        "baseUrl",
        "Base URL",
        "",
        "",
        Applies::Next,
        with(ProviderEdit::BaseUrl),
    );
    base.placeholder = "https://api.example.com/v1".to_owned();
    let mut key = TextRow::new(
        "apiKey",
        "API key",
        "",
        "Kept in the same file the desktop app uses.",
        Applies::Next,
        with(ProviderEdit::ApiKey),
    );
    key.secret = true;
    let (format_id, enabled_id) = (id.to_owned(), id.to_owned());
    let mut rows = vec![
        Row::Text(TextRow::new(
            "name",
            "Name",
            p.name.clone(),
            "",
            Applies::Next,
            with(ProviderEdit::Name),
        )),
        Row::Choice {
            key: "format".to_owned(),
            label: "API format".to_owned(),
            options: FORMATS
                .iter()
                .map(|(v, n, d)| Opt::new(*v, *n).desc(*d))
                .collect(),
            current: String::new(),
            default: String::new(),
            desc: "The kind of API the service speaks.".to_owned(),
            applies: Applies::Next,
            pick: Arc::new(move |v| edit(&format_id, ProviderEdit::Format(v.to_owned()))),
        },
        Row::Text(base),
        Row::Text(key),
        Row::Switch {
            key: "enabled".to_owned(),
            label: "Enabled".to_owned(),
            on: true,
            desc: String::new(),
            applies: Applies::Next,
            set: Arc::new(move |on| edit(&enabled_id, ProviderEdit::Enabled(on))),
        },
        Row::Heading("Models".to_owned()),
    ];
    rows.extend(
        p.models
            .iter()
            .map(|x| model_item(id, x, "⏎ opens its settings and the connection test.")),
    );
    rows.push(Row::danger(
        "deleteProvider",
        "Delete provider",
        Act::Confirm {
            label: "Delete provider".to_owned(),
            note: "This can't be undone.".to_owned(),
            next: Box::new(Act::Write(
                Write::new(
                    Change::DeleteProvider {
                        provider: id.to_owned(),
                    },
                    Applies::Next,
                )
                .saying(format!("Deleted {}.", p.name))
                .going_back(),
            )),
        },
    ));
    if let Some(Row::Action { desc, .. }) = rows.last_mut() {
        *desc =
            "Its models leave /model. Sessions that use one fail on their next request.".to_owned();
    }
    View::new(
        vec![p.name.clone()],
        "Your provider. ZCode sends requests to the address below with your key.",
        rows,
    )
}

/// 新 provider（Puffin `new_provider`）：两边都没有模板，只有自己描述的那一项。
pub(super) fn new_provider() -> View {
    View::new(
        vec!["New provider".to_owned()],
        "Start from a service you know, or describe your own.",
        vec![Row::plus(
            "custom",
            "Custom provider",
            "Name, API format, base URL and key, then its models.",
            Act::Push(Page::ProviderForm),
        )],
    )
}

/// 给新 provider 起名（Puffin `provider_form`，没有模板时的样子）。
pub(super) fn provider_form(ctx: &Ctx<'_>) -> View {
    let mut field = TextRow::new(
        "np.name",
        "Name",
        ctx.field("np.name", ""),
        "",
        Applies::Form,
        |v| Act::SetForm {
            key: "np.name".to_owned(),
            value: v.to_owned(),
        },
    );
    field.placeholder = "My provider".to_owned();
    View::new(
        vec!["New provider".to_owned(), "Custom provider".to_owned()],
        "Name it; the next page has its address, key and models.",
        vec![
            Row::Text(field),
            Row::action("np.add", "Add provider", "", Act::SubmitProvider),
        ],
    )
}

/// 一个模型：它的 id、上下文与连接测试（Puffin `model` 的内置模型）。先找你的 provider，再找内置的家族，同 Puffin。
pub(super) fn model(pid: &str, mid: &str, ctx: &Ctx<'_>) -> View {
    let owner = ctx.models().and_then(|m| {
        m.providers
            .iter()
            .find(|p| p.id == pid)
            .map(|p| (p.name.as_str(), p.models.as_slice()))
            .or_else(|| {
                m.families
                    .iter()
                    .find(|f| f.provider == pid)
                    .map(|f| (f.name.as_str(), f.models.as_slice()))
            })
    });
    let Some((name, m)) = owner.and_then(|(n, ms)| ms.iter().find(|m| m.id == mid).map(|m| (n, m)))
    else {
        return View::loading(mid);
    };
    let mut rows = vec![Row::info("Model ID", vec![Part::new(mid, Ink::Fg2)])];
    if let Some(n) = m.context_window {
        rows.push(Row::info(
            "Context window",
            vec![Part::new(format!("{} tokens", grouped(n)), Ink::Fg2)],
        ));
    }
    rows.push(Row::action(
        "test",
        "Test connection",
        "Sends one short request with your key.",
        Act::Test {
            provider: pid.to_owned(),
            model: mid.to_owned(),
        },
    ));
    View::new(
        vec![name.to_owned(), mid.to_owned()],
        "Built-in model. Its limits come with the provider.",
        rows,
    )
}

#[cfg(test)]
#[path = "providers_tests.rs"]
mod tests;
