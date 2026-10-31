//! 安装方式（docs/specs/rust-cli/update.md §2，Codex `install-context` 与 `update_action.rs`）：npm 启动器设的
//! 标记与包名。二进制只认这些变量，不看自己的路径；没有标记就是未知安装。

/// 包管理器的标记，按 Codex `InstallContext::current` 的先后（没有 Vite+）。
const MARKERS: [(&str, Manager); 3] = [
    (
        rust_cli_base::names::env::ZCODE_MANAGED_BY_PNPM,
        Manager::Pnpm,
    ),
    (
        rust_cli_base::names::env::ZCODE_MANAGED_BY_NPM,
        Manager::Npm,
    ),
    (
        rust_cli_base::names::env::ZCODE_MANAGED_BY_BUN,
        Manager::Bun,
    ),
];
/// 入口包名（启动器自己的 `package.json` 的 `name`）。
const PACKAGE: &str = rust_cli_base::names::env::ZCODE_MANAGED_PACKAGE;
/// 启动器选中的本机平台包名。
const PLATFORM_PACKAGE: &str = rust_cli_base::names::env::ZCODE_MANAGED_PLATFORM_PACKAGE;
/// npm 包名的长度上限。
const MAX_NAME: usize = 214;
/// npm 包页面（选择屏与会话提示里的 release notes 链接，update.md §6.1）。
const PACKAGE_PAGE: &str = "https://www.npmjs.com/package/";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Manager {
    Npm,
    Pnpm,
    Bun,
}

/// 由包管理器管理的安装：包管理器、入口包与（可以没有的）本机平台包。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Install {
    pub(crate) manager: Manager,
    pub(crate) package: String,
    pub(crate) platform_package: Option<String>,
}

impl Install {
    /// 从入口的环境快照判定。没有标记、入口包名缺失或不合法、平台包名不合法时为 None（未知安装）。
    pub(crate) fn detect(env: &[(String, String)]) -> Option<Self> {
        let lookup = |name: &str| rust_cli_base::secret::env_var(env, name);
        let manager = MARKERS
            .iter()
            .find(|(name, _)| lookup(name).is_some())
            .map(|(_, manager)| *manager)?;
        let package = lookup(PACKAGE).filter(|name| valid_package_name(name));
        let platform = lookup(PLATFORM_PACKAGE).filter(|name| !name.is_empty());
        let Some(package) = package else {
            tracing::warn!(
                variable = PACKAGE,
                "update: the launcher marker has no valid package name"
            );
            return None;
        };
        if platform.is_some_and(|name| !valid_package_name(name)) {
            tracing::warn!(
                variable = PLATFORM_PACKAGE,
                "update: the platform package name is not valid"
            );
            return None;
        }
        Some(Self {
            manager,
            package: package.to_owned(),
            platform_package: platform.map(str::to_owned),
        })
    }

    /// 包管理器的程序名。
    pub(crate) fn program(&self) -> &'static str {
        match self.manager {
            Manager::Npm => "npm",
            Manager::Pnpm => "pnpm",
            Manager::Bun => "bun",
        }
    }

    /// 安装命令的参数（Codex `UpdateAction::command_args`）。
    pub(crate) fn args(&self) -> Vec<String> {
        let verb: &[&str] = match self.manager {
            Manager::Npm | Manager::Bun => &["install", "-g"],
            Manager::Pnpm => &["add", "-g"],
        };
        verb.iter()
            .map(|word| (*word).to_owned())
            .chain(std::iter::once(self.package.clone()))
            .collect()
    }

    /// 显示用的命令行：程序名与参数用一个空格连起来（包名已校验，没有要转义的字符）。
    pub(crate) fn command_line(&self) -> String {
        std::iter::once(self.program().to_owned())
            .chain(self.args())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// 入口包的 npm 页面。
    pub(crate) fn notes_url(&self) -> String {
        format!("{PACKAGE_PAGE}{}", self.package)
    }
}

/// npm 包名：`@scope/name` 或 `name`，每段由小写字母、数字与 `-` `.` `_` `~` 组成、首字符是字母或数字，总长
/// 不超过 214。拿去拼命令行与 URL，所以从严。
pub(crate) fn valid_package_name(name: &str) -> bool {
    let segment = |part: &str| {
        part.chars()
            .next()
            .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
            && part.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '.' | '_' | '~')
            })
    };
    if name.is_empty() || name.len() > MAX_NAME {
        return false;
    }
    match name.strip_prefix('@') {
        Some(scoped) => match scoped.split_once('/') {
            Some((scope, rest)) => segment(scope) && segment(rest),
            None => false,
        },
        None => segment(name),
    }
}

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;
