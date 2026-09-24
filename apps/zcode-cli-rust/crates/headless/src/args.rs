//! Top-level arguments as Node parses them (`cli/src/arguments.ts` with
//! `util.parseArgs` strict semantics, validation order of `cli/src/run.ts`).
//! Spec rust-m4-headless §2.
use crate::tokens::{extract_disallowed, parse_values};

/// A usage error; `help` appends a blank line and the help text (Node `+help`).
#[derive(Debug, PartialEq, Eq)]
pub struct ArgError {
    pub message: String,
    pub help: bool,
}

pub(crate) fn fail<T>(message: impl Into<String>) -> Result<T, ArgError> {
    Err(ArgError {
        message: message.into(),
        help: false,
    })
}

pub(crate) fn parse_error<T>(message: impl Into<String>) -> Result<T, ArgError> {
    Err(ArgError {
        message: message.into(),
        help: true,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Text,
    Json,
    StreamJson,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resume {
    New,
    Id(String),
    Latest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptArgs {
    /// Untrimmed; emptiness is checked after `--cwd` ([`check_prompt`]).
    pub prompt: String,
    pub format: Format,
    pub mode: String,
    pub attach: Vec<String>,
    pub cwd: Option<String>,
    pub resume: Resume,
    pub disallowed_tools: Vec<String>,
    /// `terminal` or `desktop`.
    pub surface: String,
    pub verbose: bool,
    pub data_dir: Option<String>,
    pub config: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    Help,
    Version,
    Prompt(Box<PromptArgs>),
    /// No prompt: the (reserved) TUI.
    Tui,
}

pub fn parse(argv: &[String]) -> Result<Parsed, ArgError> {
    let (args, disallowed_tools) = extract_disallowed(argv)?;
    let (values, positionals) = parse_values(args)?;
    if let Some(locale) = values.string("locale")
        && !matches!(locale, "en-US" | "zh-CN" | "auto")
    {
        return fail(format!(
            "Unsupported --locale value: {locale}. Supported locales: en-US, zh-CN, auto."
        ));
    }
    let mode = match values.string("mode").map(str::to_lowercase) {
        None => "yolo".to_owned(),
        Some(mode) if matches!(mode.as_str(), "build" | "edit" | "plan" | "yolo") => mode,
        Some(_) => {
            let raw = values.string("mode").unwrap_or_default();
            return fail(format!(
                "Unsupported --mode value: {raw}. Supported modes: build, edit, plan, yolo."
            ));
        }
    };
    let browser = values.string("browser-use");
    if let Some(value) = browser.filter(|v| !v.eq_ignore_ascii_case("headless")) {
        return fail(format!(
            "Unsupported --browser-use value: {value}. Supported value: headless."
        ));
    }
    let surface = match values.string("surface").map(str::to_lowercase).as_deref() {
        None | Some("terminal") => "terminal".to_owned(),
        Some("desktop") => "desktop".to_owned(),
        Some(_) => {
            let raw = values.string("surface").unwrap_or_default();
            return fail(format!(
                "Unsupported --surface value: {raw}. Supported surfaces: terminal, desktop."
            ));
        }
    };
    if values.string("browser-executable").is_some() && browser.is_none() {
        return fail("--browser-executable requires --browser-use=headless.");
    }
    let resume = match (values.string("resume"), values.flag("continue")) {
        (Some(_), true) => return fail("--resume and --continue cannot be used together."),
        (Some(id), false) => Resume::Id(id.to_owned()),
        (None, true) => Resume::Latest,
        (None, false) => Resume::New,
    };
    let format = match values.string("output-format") {
        None if values.flag("json") => Format::Json,
        None | Some("text") => Format::Text,
        Some("json") => Format::Json,
        Some("stream-json") => Format::StreamJson,
        Some(other) => {
            return fail(format!(
                "--output-format must be one of text, json, stream-json (received: {other})."
            ));
        }
    };
    let prompt = values.string("prompt").map(str::to_owned);
    match values.string("target") {
        None if values.flag("target-replace") => {
            return fail("--target-replace requires --target.");
        }
        Some(target) if target.trim().is_empty() => {
            return fail("--target requires non-empty text.");
        }
        Some(_) if prompt.is_some() => {
            return fail(
                "--target cannot be used with --prompt. Use either --target <objective> or --prompt \"/goal <objective>\".",
            );
        }
        _ => {}
    }
    let targeted = values.string("target").is_some();
    if values.string("surface").is_some() && prompt.is_none() && !targeted {
        return fail(
            "--surface can only be used with --prompt, --target, app-server, or agent-server.",
        );
    }
    if values.flag("help") {
        return Ok(Parsed::Help);
    }
    if values.flag("version") {
        return Ok(Parsed::Version);
    }
    if values.flag("memory-bench") && (prompt.is_none() || !positionals.is_empty()) {
        return fail("--memory-bench can only be used with -p/--prompt.");
    }
    let scoped = prompt.is_some() || targeted || positionals.first().is_none_or(|p| p == "tui");
    if browser.is_some() && !scoped {
        return fail("--browser-use=headless can only be used with --prompt, --target, or tui.");
    }
    if values.flag("force-mcs") && !scoped {
        return fail("--force-mcs can only be used with --prompt, --target, or tui.");
    }
    // Rust 尚未实现的能力显式报错，不静默忽略。
    for (set, option) in [
        (targeted, "--target"),
        (browser.is_some(), "--browser-use"),
        (values.flag("memory-bench"), "--memory-bench"),
        (values.flag("force-mcs"), "--force-mcs"),
    ] {
        if set {
            return fail(format!(
                "{option} is not supported by the Rust runtime yet."
            ));
        }
    }
    let Some(prompt) = prompt else {
        return Ok(Parsed::Tui);
    };
    Ok(Parsed::Prompt(Box::new(PromptArgs {
        prompt,
        format,
        mode,
        attach: values.strings.get("attach").cloned().unwrap_or_default(),
        cwd: values.string("cwd").map(str::to_owned),
        resume,
        disallowed_tools,
        surface,
        verbose: values.flag("verbose"),
        data_dir: values.string("data-dir").map(str::to_owned),
        config: values.string("config").map(str::to_owned),
    })))
}

/// Node `prompt-command.ts`: checked after `--cwd`.
pub fn check_prompt(prompt: &str) -> Result<(), ArgError> {
    if prompt.trim().is_empty() {
        return fail("--prompt requires non-empty text.");
    }
    Ok(())
}

#[cfg(test)]
#[path = "args_tests.rs"]
mod tests;
