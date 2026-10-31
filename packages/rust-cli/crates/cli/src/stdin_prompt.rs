//! `-p` 的输入（docs/specs/rust-cli/cli-args.md §2.3）。
//!
//! 标准输入不是终端时读它：3 秒内没有任何数据也没有结束就不再等，写一行警告；有数据就读到结束（上限 10 MiB）。
//! 结果是 `[prompt, 读到的文字]` 去掉空的，用换行连接。Harbor 的命令行就是把任务从管道送进来、不给 prompt 参数。

use std::time::Duration;

use rust_cli_base::units;
use tokio::io::{AsyncRead, AsyncReadExt};

/// 管道输入的上限。
const LIMIT: usize = 10 * units::MIB;
/// 第一次读的缓冲区大小。
const FIRST_READ_BYTES: usize = 64 * units::KIB;
/// 等第一批数据的时间。
pub(crate) const FIRST_DATA: Duration = Duration::from_secs(3);

pub(crate) const NO_DATA_WARNING: &str = "Warning: no stdin data received in 3s, proceeding without it. If piping from a slow command, redirect stdin explicitly: < /dev/null to skip, or wait longer.";
pub(crate) const NO_INPUT: &str =
    "Error: Input must be provided either through stdin or as a prompt argument when using --print";

/// 读管道输入的结果。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Piped {
    Text(String),
    /// 3 秒内既没有数据也没有结束。
    Silent,
    TooLarge,
}

/// 读管道输入（`input` 是标准输入，`wait` 是 [`FIRST_DATA`]；测试里换成别的）。
pub(crate) async fn read_piped(
    mut input: impl AsyncRead + Unpin,
    wait: Duration,
) -> std::io::Result<Piped> {
    let mut first = vec![0u8; FIRST_READ_BYTES];
    let read = match tokio::time::timeout(wait, input.read(&mut first)).await {
        Ok(read) => read?,
        Err(_) => return Ok(Piped::Silent),
    };
    let mut bytes = first[..read].to_vec();
    if read > 0 {
        let mut rest = Vec::new();
        // 多读一个字节就知道超没超上限，不必读完整个管道。
        let budget = u64::try_from(LIMIT + 1 - bytes.len().min(LIMIT)).unwrap_or(u64::MAX);
        (&mut input).take(budget).read_to_end(&mut rest).await?;
        bytes.extend(rest);
    }
    if bytes.len() > LIMIT {
        return Ok(Piped::TooLarge);
    }
    Ok(Piped::Text(String::from_utf8_lossy(&bytes).into_owned()))
}

/// `[prompt, data].filter(Boolean).join("\n")`。
pub(crate) fn combine(prompt: Option<String>, piped: Option<String>) -> String {
    [prompt, piped]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// 管道输入超过上限时的报错（退出码 1）。
pub(crate) fn too_large() -> String {
    format!(
        "Error: piped stdin input exceeds {}MB. Pass large content as a file path in your prompt instead.",
        LIMIT / units::MIB
    )
}

/// `-p` 的输入（cli-args.md §2.3）：标准输入不是终端时读它，与 prompt 参数用换行连接；两样都
/// 没有时报错退出。
pub(crate) async fn print_prompt(prompt: Option<String>) -> anyhow::Result<String> {
    use std::io::IsTerminal;
    let piped = if std::io::stdin().is_terminal() {
        None
    } else {
        match read_piped(tokio::io::stdin(), FIRST_DATA).await {
            Ok(Piped::Text(text)) => Some(text),
            Ok(Piped::Silent) => {
                crate::output::eprint_line(NO_DATA_WARNING);
                None
            }
            Ok(Piped::TooLarge) => {
                return Err(crate::flags::Fatal(too_large()).into());
            }
            // 读不了标准输入时照常用 prompt 参数（只写日志）。
            Err(error) => {
                tracing::warn!(%error, "stdin could not be read");
                None
            }
        }
    };
    let prompt = combine(prompt, piped);
    if prompt.is_empty() {
        return Err(crate::flags::Fatal(NO_INPUT.to_owned()).into());
    }
    Ok(prompt)
}

#[cfg(test)]
#[path = "stdin_prompt_tests.rs"]
mod tests;
