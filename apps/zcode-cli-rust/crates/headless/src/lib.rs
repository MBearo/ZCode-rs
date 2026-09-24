//! Headless frontend: `zcode-cli-rust -p <prompt>` (spec rust-m4-headless).
//!
//! Like the App Server it only talks to the runtime through the transport
//! contract (`zcode_cli_core_api::{ClientMsg, ServerMsg}`); it never owns
//! session, model or tool facts.
pub mod args;
mod driver;
mod output;
mod tokens;

pub use driver::{Interrupt, Io, Request, run};

/// `--attach` kinds by extension (Node `prompt-command.ts` `attachmentKind`),
/// as the V4 attachment MIME the runtime snapshots.
pub fn attachment_mime(path: &str) -> &'static str {
    let extension = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match extension.as_str() {
        "gif" => "image/gif",
        "jpeg" | "jpg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "mp4" => "video/mp4",
        "m4v" => "video/x-m4v",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "pdf" => "application/pdf",
        // 其余按文件发送：运行时按内容决定注入文本还是二进制说明。
        _ => "application/octet-stream",
    }
}

/// Exit code of a signal (Node `shutdown.ts`).
pub fn signal_exit_code(signal: &str) -> i32 {
    match signal {
        "SIGHUP" => 129,
        "SIGINT" => 130,
        _ => 143,
    }
}

/// `--help` (Node `i18n` en-US help, limited to what this runtime supports).
pub fn help(version: &str) -> String {
    format!(
        "zcode-cli-rust {version}

Usage:
  zcode-cli-rust [command] [options]

With no command and no --prompt, the terminal UI would open (not implemented yet).

Commands:
  app-server Run the ZCode Protocol stdio app server
  tui        Open the terminal UI (not implemented yet)

Options:
  -h, --help       Show help
  -v, --version    Show version
  -p, --prompt <text>  Run a single prompt without opening the TUI
  --output-format <format>  Output format for --prompt: text, json, or stream-json
  --surface <surface>  Presentation surface for headless prompts: terminal or desktop
  --attach <path>  Attach a local file to --prompt; repeat for multiple files
  --cwd <path>     Run this command from the given directory
  --disallowed-tools, --disallowedTools <tools...>
    Remove whole tools for this prompt run only; saved settings are unchanged.
    Comma or space-separated tool names, e.g. \"Bash Edit\".
    \"Bash(git *)\" removes all of Bash; command patterns are not matched.
  --locale <locale>  UI locale: en-US, zh-CN, or auto
  --mode <mode>    Permission mode for prompts: build, edit, plan, or yolo (default: yolo for --prompt)
  --resume <sessionId>  Resume a persisted session by sessionId
  -c, --continue        Resume the latest session for the current directory
  --json           Print machine-readable JSON where supported
  --verbose        Print extra diagnostic detail
  --data-dir <path>  Rust runtime data directory
  --config <path>  Static model configuration (instead of the provider registry)
"
    )
}
