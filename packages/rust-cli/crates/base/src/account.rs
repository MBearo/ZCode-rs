//! Coding Plan 的账号体系（Provider 配置的 `accountType`，docs/specs/rust-cli/platform.md §5）。平台层按它换令牌与登录，
//! TUI 的登录视图按它列选项与写文字；名字只定义在这里（F133，审查 E7：以前在 platform，TUI 为它依赖整个 platform）。

/// Coding Plan 的账号体系（`accountType`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    Zai,
    Bigmodel,
}

impl Family {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Zai => "zai",
            Self::Bigmodel => "bigmodel",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "zai" => Some(Self::Zai),
            "bigmodel" => Some(Self::Bigmodel),
            _ => None,
        }
    }

    /// 显示名（Node `providerName`）：目录项的供应商名（platform.md §9.3）与 TUI 的登录文字都用它，中英文相同。
    /// F58：以前 platform（`account::vendor`）与 tui（`i18n_account::provider_name`）各写一份。
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Zai => "Z.AI",
            Self::Bigmodel => "BigModel",
        }
    }
}

#[cfg(test)]
#[path = "account_tests.rs"]
mod tests;
