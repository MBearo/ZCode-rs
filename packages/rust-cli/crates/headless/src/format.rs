//! `-p` 的输出与输入格式（docs/specs/rust-cli/headless.md §1）。参数的解析与报错文字在入口（cli）。

/// `--output-format`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// 最后一个 result 的文字（缺省）。
    #[default]
    Text,
    /// 结束时一个 result 对象（`--verbose` 时是全部事件的数组）。
    Json,
    /// 每行一个事件。
    StreamJson,
}

/// `--input-format`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputFormat {
    /// prompt 来自参数（与 stdin，F40）。
    #[default]
    Text,
    /// stdin 每行一条 user 消息（headless.md §4）。
    StreamJson,
}

/// 输出的选择：格式加上 `--verbose` 与 `--include-partial-messages`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PrintFormat {
    pub output: OutputFormat,
    pub verbose: bool,
    /// `--include-partial-messages`：加 `stream_event` 事件（headless.md §3.7）。
    pub partial: bool,
}

impl PrintFormat {
    /// 逐行写事件。
    pub(crate) fn streams(self) -> bool {
        self.output == OutputFormat::StreamJson
    }

    /// 结束时要全部事件（`json` 加 `--verbose`）。
    pub(crate) fn collects(self) -> bool {
        self.output == OutputFormat::Json && self.verbose
    }
}
