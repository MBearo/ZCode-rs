use super::*;

fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

#[test]
fn no_marker_is_an_unknown_install() {
    assert_eq!(Install::detect(&env(&[])), None);
    // 只有包名、没有标记：仍是未知安装（本地构建被别的进程带上包名也不检查）。
    assert_eq!(
        Install::detect(&env(&[("ZCODE_MANAGED_PACKAGE", "@scope/zcode")])),
        None
    );
}

#[test]
fn the_markers_follow_the_pnpm_npm_bun_precedence() {
    let with = |markers: &[&str]| {
        let mut pairs: Vec<(&str, &str)> = markers.iter().map(|name| (*name, "1")).collect();
        pairs.push(("ZCODE_MANAGED_PACKAGE", "@scope/zcode"));
        Install::detect(&env(&pairs)).map(|install| install.manager)
    };
    assert_eq!(with(&["ZCODE_MANAGED_BY_NPM"]), Some(Manager::Npm));
    assert_eq!(with(&["ZCODE_MANAGED_BY_PNPM"]), Some(Manager::Pnpm));
    assert_eq!(with(&["ZCODE_MANAGED_BY_BUN"]), Some(Manager::Bun));
    // pnpm 先于 npm，npm 先于 bun；只看有没有设，值可以是空串。
    assert_eq!(
        with(&["ZCODE_MANAGED_BY_NPM", "ZCODE_MANAGED_BY_PNPM"]),
        Some(Manager::Pnpm)
    );
    assert_eq!(
        with(&["ZCODE_MANAGED_BY_BUN", "ZCODE_MANAGED_BY_NPM"]),
        Some(Manager::Npm)
    );
    let empty = Install::detect(&env(&[
        ("ZCODE_MANAGED_BY_NPM", ""),
        ("ZCODE_MANAGED_PACKAGE", "zcode"),
    ]));
    assert_eq!(empty.map(|install| install.manager), Some(Manager::Npm));
}

#[test]
fn the_package_names_come_from_the_launcher_and_are_checked() {
    let install = Install::detect(&env(&[
        ("ZCODE_MANAGED_BY_NPM", "1"),
        ("ZCODE_MANAGED_PACKAGE", "@scope/zcode"),
        (
            "ZCODE_MANAGED_PLATFORM_PACKAGE",
            "@scope/zcode-darwin-arm64",
        ),
    ]))
    .unwrap();
    assert_eq!(install.package, "@scope/zcode");
    assert_eq!(
        install.platform_package.as_deref(),
        Some("@scope/zcode-darwin-arm64")
    );
    // 没有平台包名时只核对入口包。
    let entry_only = Install::detect(&env(&[
        ("ZCODE_MANAGED_BY_NPM", "1"),
        ("ZCODE_MANAGED_PACKAGE", "zcode"),
    ]))
    .unwrap();
    assert_eq!(entry_only.platform_package, None);
    for (package, platform) in [
        (None, None),
        (Some(""), None),
        (Some("@Scope/ZCode"), None),
        (Some("zcode; rm -rf /"), None),
        (Some("@scope/zcode"), Some("../zcode")),
    ] {
        let mut pairs = vec![("ZCODE_MANAGED_BY_NPM", "1")];
        if let Some(package) = package {
            pairs.push(("ZCODE_MANAGED_PACKAGE", package));
        }
        if let Some(platform) = platform {
            pairs.push(("ZCODE_MANAGED_PLATFORM_PACKAGE", platform));
        }
        assert_eq!(
            Install::detect(&env(&pairs)),
            None,
            "{package:?} {platform:?}"
        );
    }
}

#[test]
fn package_names_follow_the_npm_rules() {
    for good in [
        "zcode",
        "@scope/zcode",
        "@mbearo/zcode-win32-x64",
        "a.b_c~d-e",
        "1zcode",
    ] {
        assert!(valid_package_name(good), "{good}");
    }
    let long = "a".repeat(215);
    for bad in [
        "",
        "@scope",
        "@scope/",
        "@/zcode",
        "Zcode",
        ".zcode",
        "_zcode",
        "zcode/x",
        "@scope/zcode/x",
        "zcode x",
        "zcode\"",
        long.as_str(),
    ] {
        assert!(!valid_package_name(bad), "{bad}");
    }
}

#[test]
fn each_manager_has_its_install_command() {
    let install = |manager| Install {
        manager,
        package: "@scope/zcode".to_owned(),
        platform_package: None,
    };
    assert_eq!(
        install(Manager::Npm).command_line(),
        "npm install -g @scope/zcode"
    );
    assert_eq!(
        install(Manager::Pnpm).command_line(),
        "pnpm add -g @scope/zcode"
    );
    assert_eq!(
        install(Manager::Bun).command_line(),
        "bun install -g @scope/zcode"
    );
    assert_eq!(install(Manager::Pnpm).program(), "pnpm");
    assert_eq!(install(Manager::Pnpm).args(), ["add", "-g", "@scope/zcode"]);
    assert_eq!(
        install(Manager::Npm).notes_url(),
        "https://www.npmjs.com/package/@scope/zcode"
    );
}
