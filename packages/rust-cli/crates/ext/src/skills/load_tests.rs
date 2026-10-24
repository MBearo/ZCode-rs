//! SKILL.md 的解析与目录发现（ext.md §2.1、§2.2）。

use std::sync::Arc;

use rust_cli_kernel_api::testing::MemoryEnvironment;

use super::*;
use crate::test_support::{CWD, HOME, facts, workspace};

const GREET: &str = "---\ndescription: Greet someone by name\nwhen_to_use: When the user asks for a greeting\nargument-hint: <name>\narguments: [who]\nallowed-tools: Bash(echo:*), Read\n---\nSay hello to $ARGUMENTS.\n";

#[test]
fn frontmatter_fields_are_parsed() {
    let skill = parse(GREET, "greet".to_owned(), "/s/greet", SkillSource::Project);
    assert_eq!(skill.description, "Greet someone by name");
    assert_eq!(
        skill.when_to_use.as_deref(),
        Some("When the user asks for a greeting")
    );
    assert_eq!(skill.argument_hint.as_deref(), Some("<name>"));
    assert_eq!(skill.arg_names, ["who"]);
    assert_eq!(skill.allowed_tools, ["Bash(echo:*)", "Read"]);
    assert_eq!(skill.body, "Say hello to $ARGUMENTS.\n");
    assert!(!skill.is_safe());
    assert!(skill.user_invocable && !skill.disable_model_invocation);
}

#[test]
fn a_missing_description_comes_from_the_first_line() {
    let plain = parse(
        "# Plain skill\n\nNo frontmatter here.\n",
        "plain".to_owned(),
        "/s",
        SkillSource::User,
    );
    assert_eq!(plain.description, "Plain skill");
    assert_eq!(plain.body, "# Plain skill\n\nNo frontmatter here.\n");
    assert!(plain.is_safe());
    let long = format!("---\nname: X\n---\n{}\n", "a".repeat(120));
    let skill = parse(&long, "x".to_owned(), "/s", SkillSource::User);
    assert_eq!(skill.description, format!("{}...", "a".repeat(97)));
    let empty = parse(
        "---\nname: Y\n---\n",
        "y".to_owned(),
        "/s",
        SkillSource::User,
    );
    assert_eq!(empty.description, "Skill");
}

#[test]
fn flags_paths_and_hooks() {
    let raw = "---\ndisable-model-invocation: true\nuser-invocable: false\npaths:\n  - src/**\n  - \"**/*.py\"\nshell: powershell\ncontext: fork\nmodel: inherit\nhooks:\n  PreToolUse:\n    - matcher: Bash\n---\nBody\n";
    let skill = parse(raw, "s".to_owned(), "/s", SkillSource::Project);
    assert!(skill.disable_model_invocation && !skill.user_invocable);
    assert_eq!(
        skill.paths,
        Some(vec!["src".to_owned(), "**/*.py".to_owned()])
    );
    assert_eq!(skill.shell, SkillShell::PowerShell);
    assert!(skill.fork && skill.has_hooks && !skill.is_safe());
    assert_eq!(skill.model, None);
    let all = parse(
        "---\npaths: \"**\"\n---\nB\n",
        "a".to_owned(),
        "/s",
        SkillSource::Project,
    );
    assert_eq!(all.paths, None);
}

#[test]
fn tool_lists_split_outside_parentheses() {
    let list = tool_list(Some(&Value::Text(
        "Bash(git status:*), Read Edit".to_owned(),
    )));
    assert_eq!(list, ["Bash(git status:*)", "Read", "Edit"]);
}

fn skill_file(dir: &str, name: &str, description: &str) -> (String, String) {
    (
        format!("{dir}/{name}/SKILL.md"),
        format!("---\ndescription: {description}\n---\nBody of {name}\n"),
    )
}

/// 发现的 skill（不含内置的 `loop`，它在 builtin_tests.rs 里测）。
fn names(skills: &[Arc<Skill>]) -> Vec<String> {
    skills
        .iter()
        .filter(|skill| skill.source != SkillSource::Builtin)
        .map(|skill| format!("{}={}", skill.name, skill.description))
        .collect()
}

#[tokio::test]
async fn sources_are_ordered_and_the_first_name_wins() {
    let mut env = MemoryEnvironment::new(facts());
    for (path, text) in [
        skill_file(&format!("{HOME}/.zcode/skills"), "shared", "user zcode"),
        skill_file(&format!("{HOME}/.agents/skills"), "shared", "user agents"),
        skill_file(&format!("{HOME}/.agents/skills"), "beta", "user beta"),
        skill_file(&format!("{CWD}/.zcode/skills"), "alpha", "project"),
        skill_file(&format!("{CWD}/.zcode/skills"), "shared", "project shared"),
        skill_file(&format!("{CWD}/.agents/skills"), "Zed", "project agents"),
        skill_file("/Users/dev/.zcode/skills/not-a-skill.md", "x", "ignored"),
        skill_file(
            "/Library/Application Support/ZCode/.zcode/skills",
            "managed",
            "managed",
        ),
    ] {
        env = env.with_file(&path, &text);
    }
    // 单个 .md 文件不是 skill；没有 SKILL.md 的目录也不是。
    env = env.with_file(&format!("{CWD}/.zcode/skills/loose.md"), "x");
    env = env.with_file(&format!("{CWD}/.zcode/skills/empty/README.md"), "x");
    let env = Arc::new(env);
    let catalog = SkillCatalog::new(env.clone(), env);
    let skills = catalog.skills(&workspace()).await;
    assert_eq!(
        names(&skills),
        [
            "managed=managed",
            "shared=user zcode",
            "beta=user beta",
            "alpha=project",
            "Zed=project agents",
        ]
    );
}

#[tokio::test]
async fn project_levels_stop_at_the_git_root() {
    let nested = format!("{CWD}/app");
    let mut env_facts = facts();
    env_facts.cwd = EnvPath::new(&nested);
    let env = MemoryEnvironment::new(env_facts)
        .with_file(&format!("{CWD}/.git/HEAD"), "ref")
        .with_file("/Users/dev/outside/.zcode/skills/far/SKILL.md", "x")
        .with_file(
            &format!("{nested}/.zcode/skills/near/SKILL.md"),
            "---\ndescription: near\n---\n",
        )
        .with_file(
            &format!("{CWD}/.zcode/skills/root/SKILL.md"),
            "---\ndescription: root\n---\n",
        );
    let env = Arc::new(env);
    let catalog = SkillCatalog::new(env.clone(), env);
    let skills = catalog.skills(&workspace()).await;
    assert_eq!(names(&skills), ["near=near", "root=root"]);
}

#[tokio::test]
async fn a_dot_git_that_is_neither_file_nor_directory_does_not_stop_the_levels() {
    // 找 git 根时只认目录与文件；FIFO 等别的种类继续往上（F58：以前任何种类都算）。
    let nested = format!("{CWD}/app");
    let mut env_facts = facts();
    env_facts.cwd = EnvPath::new(&nested);
    let env = MemoryEnvironment::new(env_facts)
        .with_special(&format!("{nested}/.git"))
        .with_file(&format!("{CWD}/.git/HEAD"), "ref")
        .with_file(
            &format!("{nested}/.zcode/skills/near/SKILL.md"),
            "---\ndescription: near\n---\n",
        )
        .with_file(
            &format!("{CWD}/.zcode/skills/root/SKILL.md"),
            "---\ndescription: root\n---\n",
        );
    let env = Arc::new(env);
    let catalog = SkillCatalog::new(env.clone(), env);
    let skills = catalog.skills(&workspace()).await;
    assert_eq!(names(&skills), ["near=near", "root=root"]);
}

/// 每一层都是 `.zcode` 在前、`.agents` 在后，层与层之间离 cwd 近的在前；同名时用户级在项目级之前
/// （ext.md §2.1，与 Node `skillRootsForBase` 的拼接顺序相同）。
#[tokio::test]
async fn each_level_reads_zcode_before_agents_and_nearer_levels_first() {
    let nested = format!("{CWD}/app");
    let mut env_facts = facts();
    env_facts.cwd = EnvPath::new(&nested);
    let mut env = MemoryEnvironment::new(env_facts).with_file(&format!("{CWD}/.git/HEAD"), "ref");
    for (path, text) in [
        skill_file(
            &format!("{HOME}/.agents/skills"),
            "home-only",
            "user agents",
        ),
        skill_file(&format!("{HOME}/.agents/skills"), "both", "user agents"),
        skill_file(&format!("{nested}/.zcode/skills"), "both", "near zcode"),
        skill_file(&format!("{nested}/.agents/skills"), "near", "near agents"),
        skill_file(&format!("{nested}/.agents/skills"), "level", "near agents"),
        skill_file(&format!("{CWD}/.zcode/skills"), "level", "root zcode"),
        skill_file(&format!("{CWD}/.zcode/skills"), "near", "root zcode"),
    ] {
        env = env.with_file(&path, &text);
    }
    let env = Arc::new(env);
    let catalog = SkillCatalog::new(env.clone(), env);
    let skills = catalog.skills(&workspace()).await;
    assert_eq!(
        names(&skills),
        [
            "both=user agents",
            "home-only=user agents",
            "level=near agents",
            "near=near agents",
        ]
    );
}

#[tokio::test]
async fn plugin_skills_are_prefixed_and_parsed_once_per_version() {
    let env = Arc::new(MemoryEnvironment::new(facts()).with_file(
        "/plugins/kit/skills/deploy/SKILL.md",
        "---\ndescription: Deploy\n---\nGo\n",
    ));
    let catalog = SkillCatalog::new(env.clone(), env.clone());
    catalog.set_plugin_roots(vec![PluginSkillRoot {
        plugin: "kit".to_owned(),
        dir: "/plugins/kit/skills".to_owned(),
    }]);
    let first = catalog.skills(&workspace()).await;
    assert_eq!(names(&first), ["kit:deploy=Deploy"]);
    assert_eq!(
        first[0].source,
        SkillSource::Plugin {
            plugin: "kit".to_owned()
        }
    );
    let reads = env.reads();
    let again = catalog.find(&workspace(), "kit:deploy").await;
    assert!(again.is_some());
    assert_eq!(env.reads(), reads, "unchanged SKILL.md is not read again");
    env.set_file(
        "/plugins/kit/skills/deploy/SKILL.md",
        "---\ndescription: Deploy v2\n---\nGo\n",
    );
    let changed = catalog.skills(&workspace()).await;
    assert_eq!(names(&changed), ["kit:deploy=Deploy v2"]);
}
