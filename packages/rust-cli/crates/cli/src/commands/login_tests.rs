//! `zcode login` / `zcode logout` 的参数（Node `login-command.ts`）。

use super::*;

fn args(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

#[test]
fn login_arguments_follow_node() {
    assert_eq!(parse(&args("-p hi")), None);
    assert_eq!(
        parse(&args("logout extra")),
        Some(Ok(Command::Logout { json: false }))
    );
    assert_eq!(
        parse(&args("logout --json")),
        Some(Ok(Command::Logout { json: true }))
    );
    assert_eq!(
        parse(&args("login")),
        Some(Ok(Command::OAuth {
            family: Family::Zai,
            no_browser: false,
            json: false,
        }))
    );
    assert_eq!(
        parse(&args("login --no-browser bigmodel")),
        Some(Ok(Command::OAuth {
            family: Family::Bigmodel,
            no_browser: true,
            json: false,
        }))
    );
    // `--json`（Node `options.json`，工作线 F47）：位置不限。
    assert_eq!(
        parse(&args("login --json")),
        Some(Ok(Command::OAuth {
            family: Family::Zai,
            no_browser: false,
            json: true,
        }))
    );
    assert_eq!(
        parse(&args("login bigmodel --json --no-browser")),
        Some(Ok(Command::OAuth {
            family: Family::Bigmodel,
            no_browser: true,
            json: true,
        }))
    );
    assert_eq!(
        parse(&args("login bigmodel-coding-plan-api-key")),
        Some(Ok(Command::ApiKey {
            family: Family::Bigmodel
        }))
    );
    for bad in [
        "login other",
        "login zai bigmodel",
        "login -coding-plan-api-key",
        // 手工 key 没有 Node 的 JSON 形状。
        "login zai-coding-plan-api-key --json",
    ] {
        assert_eq!(parse(&args(bad)), Some(Err(USAGE.to_owned())), "{bad}");
    }
    assert!(USAGE.starts_with("Usage: zcode login [zai|bigmodel] [--no-browser]"));
}

/// 缺陷 R2-15：Windows 原来用 `cmd /c start "" <url>`，地址没有经过 cmd 的转义：OAuth 地址里的 `&` 被
/// cmd 当成命令分隔符，后面的参数丢失（还会被当作命令执行）。改用 `rundll32 url,OpenURL <url>`，
/// 不经过 cmd。
#[test]
fn the_browser_command_keeps_the_whole_url() {
    let url = "https://example.com/oauth?client_id=a&state=b|c";
    let windows = browser_command(Os::Windows, url);
    assert_eq!(windows, ("rundll32", vec!["url,OpenURL", url]));
    assert_eq!(browser_command(Os::Mac, url), ("open", vec![url]));
    assert_eq!(browser_command(Os::Other, url), ("xdg-open", vec![url]));
}
