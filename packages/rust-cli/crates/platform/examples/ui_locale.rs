//! 界面语言配置的互操作辅助程序（docs/specs/rust-cli/tui.md §7.1），由
//! `scripts/rust-cli/node-interop-check.mjs` 调用：
//!
//! - `write <home> <locale>`：用 `write_ui_locale` 写 `<home>/.zcode/cli/config.json` 的 `ui.locale`，输出文件路径；
//! - `read <home>`：读同一个文件，输出 `{"locale": <值或 null>}`（与入口合并用户级配置时相同的解析）。
use std::path::Path;

use anyhow::{Context, Result, bail};
use rust_cli_paths::{StoragePaths, StorageSettings};
use rust_cli_platform::{UiLocale, read_cli_config, write_ui_locale};
use serde_json::{Value, json};

#[tokio::main(flavor = "current_thread")]
#[expect(
    clippy::disallowed_methods,
    reason = "互操作辅助程序（不是产品代码）读自己的命令行参数"
)]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths = |home: &str| {
        StoragePaths::resolve(
            &StorageSettings::default(),
            Path::new(home),
            Path::new(home),
        )
    };
    let output = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["write", home, locale] => {
            let locale = UiLocale::parse(locale).context("unknown locale")?;
            let file = write_ui_locale(&paths(home), locale).await?;
            json!({ "path": file.as_path().display().to_string() })
        }
        ["read", home] => {
            let file = read_cli_config(&paths(home)).await;
            let locale = file
                .object()
                .and_then(|object| object.get("ui")?.get("locale")?.as_str())
                .and_then(UiLocale::parse)
                .map(UiLocale::as_str);
            json!({ "locale": locale.map_or(Value::Null, |value| json!(value)) })
        }
        _ => bail!("usage: ui_locale write <home> <locale> | read <home>"),
    };
    print_json(&output);
    Ok(())
}

#[expect(clippy::print_stdout, reason = "辅助程序把结果写到标准输出")]
fn print_json(value: &Value) {
    println!("{value}");
}
