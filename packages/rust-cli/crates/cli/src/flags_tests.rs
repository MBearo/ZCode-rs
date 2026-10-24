use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::testing::MemoryEnvironment;

use super::*;

const CWD: &str = "/work";

fn memory(files: &[(&str, &str)]) -> MemoryEnvironment {
    let facts = EnvFacts {
        cwd: EnvPath::new(CWD),
        platform: "linux".to_owned(),
        os_version: "Linux".to_owned(),
        shell: "bash".to_owned(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    };
    files
        .iter()
        .fold(MemoryEnvironment::new(facts), |env, (path, text)| {
            env.with_file(path, text)
        })
}

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(CWD),
    }
}

fn place<'a>(env: &'a MemoryEnvironment, workspace: &'a WorkspaceRef) -> Place<'a> {
    Place {
        files: env,
        workspace,
        cwd: CWD,
        home: Some("/Users/dev"),
        style: PathStyle::Posix,
    }
}

fn flags(inline: Option<&str>, file: Option<&str>) -> PromptFlags {
    PromptFlags {
        inline: inline.map(str::to_owned),
        file: file.map(str::to_owned),
    }
}

/// 文件（去掉一个结尾换行）+ 空行 + 文字；一边为空时取另一边原样。
#[test]
fn file_and_inline_prompts_join_with_a_blank_line() {
    let join = |file: Option<&str>, inline: Option<&str>| {
        join_prompt(file.map(str::to_owned), inline.map(str::to_owned))
    };
    assert_eq!(
        join(Some("from file\n"), Some("inline")).as_deref(),
        Some("from file\n\ninline")
    );
    assert_eq!(
        join(Some("from file\r\n"), Some("x")).as_deref(),
        Some("from file\n\nx")
    );
    assert_eq!(
        join(Some("keep\n\n"), Some("x")).as_deref(),
        Some("keep\n\n\nx")
    );
    assert_eq!(join(Some("only\n"), None).as_deref(), Some("only\n"));
    assert_eq!(join(None, Some("only")).as_deref(), Some("only"));
    assert_eq!(join(Some(""), Some("")), None);
}

#[tokio::test]
async fn prompt_files_are_read_relative_to_the_cwd() {
    let env = memory(&[("/work/prompts/system.md", "You are a test.\n")]);
    let workspace = workspace();
    let place = place(&env, &workspace);
    let text = prompt_text(
        &place,
        &flags(Some("Be brief."), Some("prompts/system.md")),
        PromptKind::System,
    )
    .await
    .unwrap();
    assert_eq!(text.as_deref(), Some("You are a test.\n\nBe brief."));
    let inline = prompt_text(&place, &flags(Some("x"), None), PromptKind::Append)
        .await
        .unwrap();
    assert_eq!(inline.as_deref(), Some("x"));
}

/// 文件不存在时按固定的文字退出（路径是解析后的绝对路径）。
#[tokio::test]
async fn a_missing_prompt_file_stops_with_the_error_text() {
    let env = memory(&[]);
    let workspace = workspace();
    let place = place(&env, &workspace);
    let system = prompt_text(&place, &flags(None, Some("nope.md")), PromptKind::System)
        .await
        .unwrap_err();
    assert_eq!(
        system.0,
        "Error: System prompt file not found: /work/nope.md"
    );
    let append = prompt_text(&place, &flags(None, Some("/abs/a.md")), PromptKind::Append)
        .await
        .unwrap_err();
    assert_eq!(
        append.0,
        "Error: Append system prompt file not found: /abs/a.md"
    );
}

/// `--add-dir`（cli-args.md §6）：不存在的、已在工作目录里的静默丢掉；不是目录的给警告；`~` 与相对路径。
#[tokio::test]
async fn added_directories_are_filtered_and_warned() {
    let env = memory(&[
        ("/lib/shared/a.rs", "a"),
        ("/Users/dev/notes/n.md", "n"),
        ("/work/sub/s.rs", "s"),
        ("/data/file.txt", "f"),
    ]);
    let workspace = workspace();
    let place = place(&env, &workspace);
    let raw: Vec<String> = [
        "../lib/shared",
        "~/notes",
        "sub",
        "/missing",
        "/data/file.txt",
        "/lib/shared/",
    ]
    .iter()
    .map(|value| (*value).to_owned())
    .collect();
    let added = add_dirs(&place, &raw).await;
    assert_eq!(added.dirs, ["/lib/shared", "/Users/dev/notes"]);
    assert_eq!(
        added.warnings,
        ["/data/file.txt is not a directory. Did you mean to add the parent directory /data?"]
    );
}

/// `--settings`（cli-args.md §8）：`{…}` 当 JSON，否则是文件；报错文字固定。
#[tokio::test]
async fn settings_are_json_text_or_a_file() {
    let env = memory(&[
        (
            "/work/s.json",
            r#"{"permission":{"allowedTools":["Read"]}}"#,
        ),
        ("/work/bad.json", "not json"),
    ]);
    let workspace = workspace();
    let place = place(&env, &workspace);
    let inline = settings(&place, r#" {"ui":{"theme":"dark"}} "#)
        .await
        .unwrap();
    assert_eq!(inline["ui"]["theme"], "dark");
    let file = settings(&place, "s.json").await.unwrap();
    assert_eq!(file["permission"]["allowedTools"][0], "Read");
    // 文件里不是 JSON：-p 里静默忽略。
    assert!(settings(&place, "bad.json").await.unwrap().is_empty());
    assert_eq!(
        settings(&place, "{not json}").await.unwrap_err().0,
        "Error: Invalid JSON provided to --settings"
    );
    assert_eq!(
        settings(&place, "missing.json").await.unwrap_err().0,
        "Error: Settings file not found: /work/missing.json"
    );
}

/// root 下的 bypass（cli-args.md §5.2）：`IS_SANDBOX=1` 或 `ZCODE_BUBBLEWRAP` 为真时放行。文字写模式名，不写已删掉的
/// `--dangerously-skip-permissions`（工作线 F63）。
#[test]
fn bypass_as_root_needs_a_declared_sandbox() {
    let env = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    };
    let refused = refuse_root_bypass(true, Some(0), &env(&[])).unwrap_err();
    assert_eq!(
        refused.0,
        "Permission mode bypassPermissions (yolo) cannot be used with root/sudo privileges for security reasons"
    );
    assert!(refuse_root_bypass(true, Some(0), &env(&[("IS_SANDBOX", "1")])).is_ok());
    // 只认 `1`（`IS_SANDBOX !== "1"` 时拒绝）。
    assert!(refuse_root_bypass(true, Some(0), &env(&[("IS_SANDBOX", "true")])).is_err());
    assert!(refuse_root_bypass(true, Some(0), &env(&[("ZCODE_BUBBLEWRAP", "yes")])).is_ok());
    assert!(refuse_root_bypass(true, Some(0), &env(&[("ZCODE_BUBBLEWRAP", "0")])).is_err());
    assert!(refuse_root_bypass(true, Some(501), &env(&[])).is_ok());
    assert!(refuse_root_bypass(false, Some(0), &env(&[])).is_ok());
    // Windows 没有 uid。
    assert!(refuse_root_bypass(true, None, &env(&[])).is_ok());
}
