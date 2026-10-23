//! 输出：按格式写事件，记下 result（docs/specs/rust-cli/headless.md §3.2）。整个进程一个，跨回合累计
//! `modelUsage` 与 `result_index`。

use std::collections::BTreeMap;
use std::sync::Arc;

use rust_cli_kernel_api::clock::{Clock, Ids};
use rust_cli_kernel_api::message::Usage;
use serde_json::Value;
use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::events::Stamp;
use crate::format::{OutputFormat, PrintFormat};

/// 事件的 uuid 与时间的来源：入口的那一份 Ids 与 Clock（README §4）。
#[derive(Clone)]
pub struct Stamps {
    pub ids: Arc<dyn Ids>,
    pub clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for Stamps {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Stamps")
    }
}

/// 输出的设置（[`crate::PrintOptions::output`]）。
#[derive(Debug, Clone, Default)]
pub struct Output {
    pub format: PrintFormat,
    /// 没有时 uuid 为空、时间是 0（只有 text 输出的测试这样用）。
    pub stamps: Option<Stamps>,
    /// system/init 的 `zcode_version`（本工程的版本，headless.md §3.3）。
    pub version: String,
    /// system/init 的 `apiKeySource`。
    pub api_key_source: String,
}

/// 记下的最后一个 result。
struct Last {
    value: Value,
    /// text 输出（headless.md §3.2）。
    text: String,
    is_error: bool,
}

pub(crate) struct Printer<'w, W> {
    pub(crate) output: Output,
    out: &'w mut W,
    collected: Vec<Value>,
    results: usize,
    /// 本进程主线程按模型累计的用量（result 的 `modelUsage`）。
    pub(crate) model_usage: BTreeMap<String, Usage>,
    last: Option<Last>,
    /// 推迟写的 result（输入已关、还有在跑的后台子代理时，回合的 result 等到下一个
    /// result 之前再写，headless.md §3.1）。
    deferred: Vec<Last>,
}

impl<'w, W: AsyncWrite + Unpin> Printer<'w, W> {
    pub(crate) fn new(output: Output, out: &'w mut W) -> Self {
        Self {
            output,
            out,
            collected: Vec::new(),
            results: 0,
            model_usage: BTreeMap::new(),
            last: None,
            deferred: Vec::new(),
        }
    }

    pub(crate) fn format(&self) -> PrintFormat {
        self.output.format
    }

    /// 写事件：stream-json 逐行写；json 加 `--verbose` 时收起来；其余不写。
    pub(crate) async fn emit(&mut self, event: Value) -> std::io::Result<()> {
        let format = self.output.format;
        if format.streams() {
            let mut line = rust_cli_base::js::json::stringify(&event);
            line.push('\n');
            self.out.write_all(line.as_bytes()).await?;
            self.out.flush().await?;
        } else if format.collects() {
            self.collected.push(event);
        }
        Ok(())
    }

    pub(crate) fn now_ms(&self) -> u64 {
        self.output
            .stamps
            .as_ref()
            .map_or(0, |stamps| stamps.clock.now_ms())
    }

    pub(crate) fn uuid(&self) -> String {
        self.output
            .stamps
            .as_ref()
            .map(|stamps| stamps.ids.command().as_str().to_owned())
            .unwrap_or_default()
    }

    pub(crate) fn stamp(&self) -> Stamp {
        Stamp {
            uuid: self.uuid(),
            timestamp: rust_cli_base::js::iso_timestamp(self.now_ms()),
        }
    }

    /// 下一个 result 的序号（`result_index`）。
    pub(crate) fn result_index(&self) -> usize {
        self.results
    }

    /// 写一个 result 并记下它；先写推迟的。
    pub(crate) async fn result(
        &mut self,
        value: Value,
        text: String,
        is_error: bool,
    ) -> std::io::Result<()> {
        self.results += 1;
        self.flush_deferred().await?;
        self.write_result(Last {
            value,
            text,
            is_error,
        })
        .await
    }

    /// 推迟一个 result（序号照常占用），到下一个 result 之前或结束时再写。
    pub(crate) fn defer(&mut self, value: Value, text: String, is_error: bool) {
        self.results += 1;
        self.deferred.push(Last {
            value,
            text,
            is_error,
        });
    }

    async fn write_result(&mut self, last: Last) -> std::io::Result<()> {
        self.emit(last.value.clone()).await?;
        self.last = Some(last);
        Ok(())
    }

    async fn flush_deferred(&mut self) -> std::io::Result<()> {
        for last in std::mem::take(&mut self.deferred) {
            self.write_result(last).await?;
        }
        Ok(())
    }

    /// 最后一个 result 是错误（退出码 1，headless.md §5）。
    pub(crate) fn failed(&self) -> bool {
        self.last.as_ref().is_some_and(|last| last.is_error)
    }

    pub(crate) fn has_result(&self) -> bool {
        self.last.is_some() || !self.deferred.is_empty()
    }

    /// 结束：text 写最后一个 result 的文字；json 写最后一个 result（`--verbose` 时是全部事件的数组）。
    pub(crate) async fn finish(&mut self) -> std::io::Result<()> {
        self.flush_deferred().await?;
        let text = match self.output.format.output {
            OutputFormat::StreamJson => return Ok(()),
            OutputFormat::Text => match &self.last {
                Some(last) => last.text.clone(),
                None => return Ok(()),
            },
            OutputFormat::Json => {
                let value = if self.output.format.verbose {
                    Value::Array(std::mem::take(&mut self.collected))
                } else {
                    match &self.last {
                        Some(last) => last.value.clone(),
                        None => return Ok(()),
                    }
                };
                let mut line = rust_cli_base::js::json::stringify(&value);
                line.push('\n');
                line
            }
        };
        self.out.write_all(text.as_bytes()).await?;
        self.out.flush().await
    }
}
