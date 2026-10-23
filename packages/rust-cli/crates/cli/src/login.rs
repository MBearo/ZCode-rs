//! `zcode login` / `zcode logout`（docs/specs/rust-cli/platform.md §5.6）：参数、提示与输出文本取 Node
//! `apps/zcode-cli/packages/cli/src/login-command.ts`（含 `--json`，工作线 F47）；登录与写文件在平台层。

use std::io::{IsTerminal, Write};
use std::process::Stdio;

use rust_cli_base::secret::RedactedString;
use rust_cli_platform::{Family, LoginError, Platform};
use tokio::io::AsyncBufReadExt;

const USAGE: &str = "Usage: zcode login [zai|bigmodel] [--no-browser]\n       zcode login <zai|bigmodel>-coding-plan-api-key   (reads the API key from stdin)";
const JSON_FLAG: &str = "--json";

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    /// CLI OAuth 轮询登录。
    OAuth {
        family: Family,
        no_browser: bool,
        /// `--json`：结果是一段 JSON（Node `options.json`）。
        json: bool,
    },
    /// 手工 key：从标准输入读一行。
    ApiKey {
        family: Family,
    },
    Logout {
        json: bool,
    },
}

/// 第一个参数是 `login` / `logout` 时解析成命令；其他命令返回空。
pub(crate) fn parse(args: &[String]) -> Option<Result<Command, String>> {
    let (first, rest) = args.split_first()?;
    match first.as_str() {
        // Node 的 logout 不看其余位置参数。
        "logout" => Some(Ok(Command::Logout {
            json: rest.iter().any(|arg| arg == JSON_FLAG),
        })),
        "login" => Some(parse_login(rest)),
        _ => None,
    }
}

fn parse_login(args: &[String]) -> Result<Command, String> {
    let no_browser = args.iter().any(|arg| arg == "--no-browser");
    let json = args.iter().any(|arg| arg == JSON_FLAG);
    let positionals: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|arg| *arg != "--no-browser" && *arg != JSON_FLAG)
        .collect();
    let target = match positionals.as_slice() {
        [] => "zai",
        [target] => target,
        _ => return Err(USAGE.to_owned()),
    };
    let (name, api_key) = match target.strip_suffix("-coding-plan-api-key") {
        Some(name) => (name, true),
        None => (target, false),
    };
    let family = Family::parse(name).ok_or_else(|| USAGE.to_owned())?;
    Ok(match (api_key, json) {
        // 手工 key 是本工程才有的命令，Node 没有对应的 JSON 形状。
        (true, true) => return Err(USAGE.to_owned()),
        (true, false) => Command::ApiKey { family },
        (false, _) => Command::OAuth {
            family,
            no_browser,
            json,
        },
    })
}

fn display(family: Family) -> &'static str {
    match family {
        Family::Zai => "Z.AI",
        Family::Bigmodel => "BigModel",
    }
}

/// 执行命令，返回退出码。输出写到标准输出，错误写到标准错误（`Error: <message>`）。
pub(crate) async fn run(command: Command, platform: &Platform) -> u8 {
    let mut out = std::io::stdout();
    let result = match command {
        Command::OAuth {
            family,
            no_browser,
            json,
        } => oauth(platform, (family, no_browser, json), &mut out).await,
        Command::ApiKey { family } => api_key(platform, family, &mut out).await,
        Command::Logout { json } => platform.logout().await.map(|file| {
            let path = file.as_path().display().to_string();
            if json {
                // Node 写死 provider 为 zai。
                let value = serde_json::json!({
                    "status": "logged_out",
                    "provider": "zai",
                    "credentialsPath": path,
                });
                let _ = write!(out, "{}", format_json(&value));
            } else {
                let _ = writeln!(
                    out,
                    "Logged out from Coding Plan accounts. Credentials: {path}"
                );
            }
        }),
    };
    match result {
        Ok(()) => 0,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "Error: {error}");
            1
        }
    }
}

/// Node `formatJson`：`JSON.stringify(value, null, 2)` 加换行。
fn format_json(value: &serde_json::Value) -> String {
    format!(
        "{}\n",
        serde_json::to_string_pretty(value).unwrap_or_default()
    )
}

async fn oauth(
    platform: &Platform,
    (family, no_browser, json): (Family, bool, bool),
    out: &mut impl Write,
) -> Result<(), LoginError> {
    let pending = platform.begin_login(family).await?;
    let url = pending.authorize_url().to_owned();
    // `--json` 时提示写到 stderr，stdout 只留结果（Node 相同）。
    let mut stderr = std::io::stderr();
    let prompt: &mut dyn Write = if json { &mut stderr } else { out };
    let mut browser_opened = false;
    if no_browser {
        let _ = writeln!(prompt, "Open this URL to sign in:\n{url}");
    } else {
        let _ = writeln!(
            prompt,
            "Opening browser for {} authorization.\nFallback URL:\n{url}",
            display(family)
        );
        match open_browser(&url) {
            Ok(()) => browser_opened = true,
            // Node：`--json` 时打不开也不提示。
            Err(reason) if !json => {
                let _ = writeln!(prompt, "Browser open failed: {reason}");
            }
            Err(_) => {}
        }
    }
    let _ = prompt.flush();
    let outcome = pending.complete().await?;
    if json {
        let field = |name: &str| {
            outcome
                .user
                .get(name)
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
        };
        let mut user = serde_json::Map::new();
        // `JSON.stringify` 不写 undefined 的键。
        if let Some(id) = outcome.user.get("user_id") {
            user.insert("user_id".to_owned(), id.clone());
        }
        for name in ["email", "name", "avatar"] {
            if let Some(value) = field(name) {
                user.insert(name.to_owned(), serde_json::json!(value));
            }
        }
        let value = serde_json::json!({
            "status": "ready",
            "provider": family.as_str(),
            "user": user,
            "model": outcome.model,
            "credentialsPath": outcome.credentials.as_path().display().to_string(),
            "configPath": outcome.config.as_path().display().to_string(),
            "browserOpened": browser_opened,
        });
        let _ = write!(out, "{}", format_json(&value));
        return Ok(());
    }
    let label = outcome
        .user_label()
        .map(|label| format!(" as {label}"))
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "Login successful{label}.\nModel: {}\nCredentials: {}\nModel selection: {}",
        outcome.model,
        outcome.credentials.as_path().display(),
        outcome.config.as_path().display()
    );
    Ok(())
}

async fn api_key(
    platform: &Platform,
    family: Family,
    out: &mut impl Write,
) -> Result<(), LoginError> {
    // key 不放在命令行参数里（会进 shell 历史与进程列表）：从标准输入读一行。
    if std::io::stdin().is_terminal() {
        let _ = write!(std::io::stderr(), "API key: ");
        let _ = std::io::stderr().flush();
    }
    let mut line = String::new();
    let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
    if let Err(error) = reader.read_line(&mut line).await {
        return Err(LoginError::Credentials(format!(
            "cannot read the API key: {error}"
        )));
    }
    let outcome = platform
        .configure_api_key(family, &RedactedString::new(line))
        .await?;
    let _ = writeln!(
        out,
        "Configured {} Coding Plan.\nModel: {}\nModel selection: {}",
        display(outcome.family),
        outcome.model,
        outcome.config.as_path().display()
    );
    Ok(())
}

/// 打开浏览器用的系统命令属于哪一类平台。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Os {
    Mac,
    Windows,
    Other,
}

impl Os {
    fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

/// 打开地址的程序与参数（Node `browserOpenCommand`；Windows 用 `rundll32`）。
///
/// 缺陷修复 R2-15：Windows 原来按 Node 用 `cmd.exe /c start "" <url>`。cmd 会重新解析整行，地址里的 `&`、`|`
/// 被当成命令分隔符：OAuth 地址的参数丢失，后半段还会被当作命令执行。现在用
/// `rundll32 url,OpenURL <url>`，不经过 cmd，地址原样到达。
pub(crate) fn browser_command(os: Os, url: &str) -> (&'static str, Vec<&str>) {
    match os {
        Os::Mac => ("open", vec![url]),
        Os::Windows => ("rundll32", vec!["url,OpenURL", url]),
        Os::Other => ("xdg-open", vec![url]),
    }
}

/// 用系统的打开方式打开授权地址（Node `openUrlInBrowser`）：启动成功即算打开，不等它结束。
pub(crate) fn open_browser(url: &str) -> Result<(), String> {
    let (program, args) = browser_command(Os::current(), url);
    rust_cli_proc::command(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;
