//! `-p` 的格式参数（docs/specs/rust-cli/headless.md §1）：取值与报错文字（退出码 1）。

use rust_cli_headless::{InputFormat, OutputFormat, PrintFormat};

/// 格式类参数：`--output-format`、`--input-format`、`--verbose`、`--include-partial-messages`。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct FormatFlags {
    pub(crate) output: OutputFormat,
    pub(crate) input: InputFormat,
    pub(crate) verbose: bool,
    pub(crate) partial: bool,
}

impl FormatFlags {
    pub(crate) fn print_format(self) -> PrintFormat {
        PrintFormat {
            output: self.output,
            verbose: self.verbose,
            partial: self.partial,
        }
    }
}

/// `--output-format` 的值；不在 choices 里时为 None（commander 的报错在 args.rs）。
pub(crate) fn output_format(value: &str) -> Option<OutputFormat> {
    match value {
        "text" => Some(OutputFormat::Text),
        "json" => Some(OutputFormat::Json),
        "stream-json" => Some(OutputFormat::StreamJson),
        _ => None,
    }
}

/// `--input-format` 的值。
pub(crate) fn input_format(value: &str) -> Option<InputFormat> {
    match value {
        "text" => Some(InputFormat::Text),
        "stream-json" => Some(InputFormat::StreamJson),
        _ => None,
    }
}

/// 建会话之前的校验（headless.md §1.1 的 2–4）。`print` 是有没有 `-p`。
pub(crate) fn check(flags: FormatFlags, print: bool) -> Result<(), String> {
    let streams = flags.output == OutputFormat::StreamJson;
    if flags.input == InputFormat::StreamJson && !streams {
        return Err(
            "Error: --input-format=stream-json requires output-format=stream-json.".to_owned(),
        );
    }
    if flags.input == InputFormat::StreamJson && !print {
        return Err("Error: --input-format=stream-json requires --print.".to_owned());
    }
    if flags.partial && !(print && streams) {
        return Err(
            "Error: --include-partial-messages requires --print and --output-format=stream-json."
                .to_owned(),
        );
    }
    Ok(())
}

/// 读到输入之后的校验（headless.md §1.1 的 5、6，6 排在 5 之后）。`prompt` 是 text 输入
/// 的 prompt（stream-json 输入不看它）。
pub(crate) fn check_input(flags: FormatFlags, prompt: &str) -> Result<(), String> {
    if flags.input == InputFormat::Text && prompt.trim().is_empty() {
        return Err(if prompt.is_empty() {
            "Error: Input must be provided either through stdin or as a prompt argument when using --print"
        } else {
            "Error: Input contained only whitespace. Provide a prompt with text through stdin or as a prompt argument when using --print"
        }
        .to_owned());
    }
    if flags.output == OutputFormat::StreamJson && !flags.verbose {
        return Err(
            "Error: When using --print, --output-format=stream-json requires --verbose".to_owned(),
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "print_format_tests.rs"]
mod tests;
