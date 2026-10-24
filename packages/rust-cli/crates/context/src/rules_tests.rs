use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::testing::MemoryEnvironment;

use crate::rules::{Kind, RuleFile, Scope};

fn facts(cwd: &str) -> EnvFacts {
    EnvFacts {
        cwd: EnvPath::new(cwd),
        platform: "linux".to_owned(),
        os_version: String::new(),
        shell: "bash".to_owned(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    }
}

fn workspace(cwd: &str) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(cwd),
    }
}

fn scope<'a>(env: &'a MemoryEnvironment, workspace: &'a WorkspaceRef) -> Scope<'a> {
    Scope {
        files: env,
        workspace,
        style: PathStyle::Posix,
        cwd: workspace.root.as_str().to_owned(),
        home: Some("/Users/dev".to_owned()),
    }
}

fn summary(files: &[RuleFile]) -> Vec<(&str, Kind)> {
    files
        .iter()
        .map(|file| (file.path.as_str(), file.kind))
        .collect()
}

#[tokio::test]
async fn startup_loads_the_rules_in_priority_order() {
    let env = MemoryEnvironment::new(facts("/Users/dev/w"))
        .with_file("/Library/Application Support/ZCode/AGENTS.md", "managed")
        .with_file(
            "/Library/Application Support/ZCode/.zcode/rules/m.md",
            "managed rule",
        )
        .with_file("/Users/dev/.zcode/AGENTS.md", "user")
        .with_file("/Users/dev/.zcode/rules/u.md", "user rule")
        .with_file("/Users/dev/AGENTS.md", "above the workspace")
        .with_file("/Users/dev/w/AGENTS.md", "project")
        .with_file("/Users/dev/w/.zcode/AGENTS.md", "dot zcode")
        .with_file("/Users/dev/w/.zcode/rules/a.md", "rule a")
        .with_file("/Users/dev/w/.zcode/rules/sub/b.md", "rule b")
        .with_file("/Users/dev/w/.zcode/rules/note.txt", "not markdown")
        .with_file(
            "/Users/dev/w/.zcode/rules/ts.md",
            "---\npaths: src/**/*.ts\n---\nts",
        )
        .with_file("/Users/dev/w/AGENTS.local.md", "local")
        .with_file("/Users/dev/w/.agents/AGENTS.md", "not a rule location");
    let ws = workspace("/Users/dev/w");
    let files = scope(&env, &ws).startup("darwin").await;
    assert_eq!(
        summary(&files),
        [
            (
                "/Library/Application Support/ZCode/AGENTS.md",
                Kind::Managed
            ),
            (
                "/Library/Application Support/ZCode/.zcode/rules/m.md",
                Kind::Managed
            ),
            ("/Users/dev/.zcode/AGENTS.md", Kind::User),
            ("/Users/dev/.zcode/rules/u.md", Kind::User),
            // `/Users/dev` 这一层：`.zcode/AGENTS.md` 与 `.zcode/rules` 就是用户级的，已处理过。
            ("/Users/dev/AGENTS.md", Kind::Project),
            ("/Users/dev/w/AGENTS.md", Kind::Project),
            ("/Users/dev/w/.zcode/AGENTS.md", Kind::Project),
            ("/Users/dev/w/.zcode/rules/a.md", Kind::Project),
            ("/Users/dev/w/.zcode/rules/sub/b.md", Kind::Project),
            ("/Users/dev/w/AGENTS.local.md", Kind::Local),
        ]
    );
    // Linux 的系统目录是 /etc/zcode。
    let linux = scope(&env, &ws).startup("linux").await;
    assert_eq!(linux[0].path, "/Users/dev/.zcode/AGENTS.md");
}

#[tokio::test]
async fn includes_stop_at_cycles_depth_binaries_and_the_workspace_edge() {
    let env = MemoryEnvironment::new(facts("/w"))
        .with_file(
            "/w/AGENTS.md",
            "Start @./docs/a.md @~/outside.md @img.png\n```\n@docs/code.md\n```\n",
        )
        .with_file("/w/docs/a.md", "A @../AGENTS.md @b.md")
        .with_file("/w/docs/b.md", "B @c1.md")
        .with_file("/w/docs/c1.md", "C1 @c2.md")
        .with_file("/w/docs/c2.md", "C2 @c3.md")
        .with_file("/w/docs/c3.md", "C3 @c4.md")
        .with_file("/w/docs/c4.md", "C4 too deep")
        .with_file("/w/docs/code.md", "inside a code block")
        .with_file("/w/img.png", "binary")
        .with_file("/Users/dev/outside.md", "outside the workspace")
        .with_file("/Users/dev/.zcode/AGENTS.md", "user @~/notes.md")
        .with_file("/Users/dev/notes.md", "user notes");
    let ws = workspace("/w");
    let files = scope(&env, &ws).startup("linux").await;
    let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/Users/dev/.zcode/AGENTS.md",
            // User 类型的 include 可以在工作区外。
            "/Users/dev/notes.md",
            "/w/AGENTS.md",
            "/w/docs/a.md",
            "/w/docs/b.md",
            "/w/docs/c1.md",
            // 深度 5 起不再跟：c3 在第 5 层。
            "/w/docs/c2.md",
        ]
    );
    let a = files
        .iter()
        .find(|file| file.path == "/w/docs/a.md")
        .unwrap();
    assert_eq!(a.parent.as_deref(), Some("/w/AGENTS.md"));
    assert_eq!(a.kind, Kind::Project);
}

#[tokio::test]
async fn contents_drop_frontmatter_and_comments_and_empty_files_are_skipped() {
    let env = MemoryEnvironment::new(facts("/w"))
        .with_file(
            "/w/AGENTS.md",
            "---\ntitle: x\n---\n# Rules\r\n\r\n<!-- private -->\r\n\r\nUse tabs.\r\n",
        )
        .with_file("/w/.zcode/AGENTS.md", "<!-- only a comment -->\n\n")
        .with_file("/w/AGENTS.local.md", "  \n");
    let ws = workspace("/w");
    let files = scope(&env, &ws).startup("linux").await;
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].content, "# Rules\n\nUse tabs.\n");
    assert_eq!(files[0].globs, None);
}

#[tokio::test]
async fn a_nested_worktree_skips_checked_in_files_of_the_main_repository() {
    let env = MemoryEnvironment::new(facts("/r/.zcode/worktrees/wt"))
        .with_file("/r/AGENTS.md", "main repo project")
        .with_file("/r/AGENTS.local.md", "main repo local")
        .with_file("/r/.git/HEAD", "ref: refs/heads/main")
        .with_file("/r/.git/worktrees/wt/commondir", "../..\n")
        .with_file(
            "/r/.git/worktrees/wt/gitdir",
            "/r/.zcode/worktrees/wt/.git\n",
        )
        .with_file(
            "/r/.zcode/worktrees/wt/.git",
            "gitdir: /r/.git/worktrees/wt\n",
        )
        .with_file("/r/.zcode/worktrees/wt/AGENTS.md", "worktree project");
    let ws = workspace("/r/.zcode/worktrees/wt");
    let files = scope(&env, &ws).startup("linux").await;
    assert_eq!(
        summary(&files),
        [
            ("/r/AGENTS.local.md", Kind::Local),
            ("/r/.zcode/worktrees/wt/AGENTS.md", Kind::Project),
        ]
    );
}

#[tokio::test]
async fn a_touched_file_brings_its_directory_rules_and_matching_conditional_rules() {
    let env = MemoryEnvironment::new(facts("/w"))
        .with_file(
            "/Users/dev/.zcode/rules/u.md",
            "---\npaths: src/ui\n---\nuser ui",
        )
        .with_file(
            "/Users/dev/.zcode/rules/other.md",
            "---\npaths: lib\n---\nuser lib",
        )
        .with_file("/w/AGENTS.md", "root (already announced)")
        .with_file("/w/src/AGENTS.md", "src")
        .with_file("/w/src/ui/.zcode/AGENTS.md", "ui dot")
        .with_file("/w/src/ui/AGENTS.local.md", "ui local")
        .with_file("/w/src/ui/.zcode/rules/plain.md", "ui plain")
        .with_file(
            "/w/src/ui/.zcode/rules/cond.md",
            "---\npaths: \"*.tsx\"\n---\nui tsx",
        )
        .with_file(
            "/w/src/ui/.zcode/rules/css.md",
            "---\npaths: \"*.css\"\n---\nui css",
        )
        .with_file(
            "/w/.zcode/rules/tsx.md",
            "---\npaths: src/**/*.tsx\n---\nroot tsx",
        )
        .with_file(
            "/w/.zcode/rules/docs.md",
            "---\npaths: docs\n---\nroot docs",
        )
        .with_file("/w/.zcode/rules/plain.md", "root plain");
    let ws = workspace("/w");
    let scope = scope(&env, &ws);
    let files = scope.nested("linux", "/w/src/ui/button.tsx", None).await;
    assert_eq!(
        summary(&files),
        [
            ("/Users/dev/.zcode/rules/u.md", Kind::User),
            ("/w/src/AGENTS.md", Kind::Project),
            ("/w/src/ui/.zcode/AGENTS.md", Kind::Project),
            ("/w/src/ui/AGENTS.local.md", Kind::Local),
            ("/w/src/ui/.zcode/rules/plain.md", Kind::Project),
            ("/w/src/ui/.zcode/rules/cond.md", Kind::Project),
            ("/w/.zcode/rules/tsx.md", Kind::Project),
        ]
    );
    // 工作区根里的文件：没有子目录，只看条件规则。
    let root = scope.nested("linux", "/w/README.md", None).await;
    assert!(root.is_empty(), "{root:?}");
}

#[tokio::test]
async fn a_gitdir_pointer_to_a_network_path_is_not_followed() {
    // `.git` 文件指向 UNC 时不读、不算 worktree（F58 之前 context 照读）。
    let env = MemoryEnvironment::new(facts("/srv/main/trees/wt"))
        .with_file("/srv/main/.git/HEAD", "ref: refs/heads/main")
        .with_file("/srv/main/.git/worktrees/wt/commondir", "../..\n")
        .with_file(
            "/srv/main/.git/worktrees/wt/gitdir",
            "/srv/main/trees/wt/.git\n",
        )
        .with_file(
            "/srv/main/trees/wt/.git",
            "gitdir: //srv/main/.git/worktrees/wt\n",
        );
    let ws = workspace("/srv/main/trees/wt");
    assert!(scope(&env, &ws).worktree().await.is_none());
    // 对照：同样的结构写成本地路径时是 worktree。
    env.set_file(
        "/srv/main/trees/wt/.git",
        "gitdir: /srv/main/.git/worktrees/wt\n",
    );
    assert!(scope(&env, &ws).worktree().await.is_some());
}
