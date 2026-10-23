// 复制自 rust/crates/kit/src/secret.rs（feat/rs 79b6691c55）。
//! 不会被意外打印的秘密值（docs/specs/rust-u0-foundation.md §3.2）。

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// API key、鉴权 header 等秘密值。
///
/// `Debug` / `Display` 只输出 `<redacted>`，把它放进日志、错误信息或 `{:?}` 都不会泄露原值；
/// serde 按原值透明地序列化与反序列化（发给 provider 时需要原值）。读取原值只能经 [`expose`](Self::expose)，
/// 调用处因此一眼可见。
#[derive(Clone, PartialEq, Eq)]
pub struct RedactedString(String);

impl RedactedString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// 原值。只在真正需要发出秘密值的地方调用（例如构造请求 header）。
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RedactedString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

impl fmt::Display for RedactedString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

impl Serialize for RedactedString {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RedactedString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self)
    }
}

/// 入口捕获的进程环境快照（README §4：cli 读一次，往下传）。值里常有凭据（`ZCODE_API_KEY` 等）。
///
/// 缺陷修复 R2-13：原来是明文的 `Vec<(String, String)>`，放在 `derive(Debug)` 的配置里，`{:?}` 会打出所有值。
/// 现在 `Debug` 只列变量名；按名字读值经 [`get`](Self::get)，按顺序遍历经 `Deref` 到切片（白名单投影等）。
#[derive(Clone, Default, PartialEq, Eq)]
pub struct EnvSnapshot(Vec<(String, String)>);

impl EnvSnapshot {
    /// 按名字取值，名字的比较见 [`same_env_name`]（本机平台）。
    pub fn get(&self, name: &str) -> Option<&str> {
        env_var(&self.0, name)
    }

    /// 把同名变量（[`same_env_name`]）的值换成 `value`，没有时加在末尾。只给入口用：启动时改写进程环境的那几处
    /// （例如 Windows 上把 `SHELL` 换成 Git Bash，F58）在快照上做一次，之后所有读取方看到同一个值。
    #[must_use]
    pub fn with_var(mut self, name: &str, value: &str) -> Self {
        match self
            .0
            .iter_mut()
            .find(|(key, _)| same_env_name(key, name, cfg!(windows)))
        {
            Some(entry) => entry.1 = value.to_owned(),
            None => self.0.push((name.to_owned(), value.to_owned())),
        }
        self
    }
}

/// 环境变量名是否相同（修复线 F58，README §4.1）：Windows 上不分大小写，其他平台逐字节相同。照 Node：Windows 上
/// `process.env.X` 经 `GetEnvironmentVariableW` 不分大小写（`Path` 就是 `PATH`），Node 版 zcode 也这样读。
/// 以前快照只按原样比较，只有代理变量与子进程的白名单各自不分大小写；入口另外直接读进程环境（不分大小写）。
pub fn same_env_name(key: &str, name: &str, windows: bool) -> bool {
    if windows {
        key.eq_ignore_ascii_case(name)
    } else {
        key == name
    }
}

/// 快照里第一个同名变量的值（[`same_env_name`]，本机平台）。入口的各个读取函数只有切片时用它；同一个规则只在这里。
pub fn env_var<'a>(vars: &'a [(String, String)], name: &str) -> Option<&'a str> {
    vars.iter()
        .find(|(key, _)| same_env_name(key, name, cfg!(windows)))
        .map(|(_, value)| value.as_str())
}

impl From<Vec<(String, String)>> for EnvSnapshot {
    fn from(vars: Vec<(String, String)>) -> Self {
        Self(vars)
    }
}

impl FromIterator<(String, String)> for EnvSnapshot {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(vars: I) -> Self {
        Self(vars.into_iter().collect())
    }
}

impl std::ops::Deref for EnvSnapshot {
    type Target = [(String, String)];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl fmt::Debug for EnvSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("EnvSnapshot")
            .field(&self.0.iter().map(|(key, _)| key).collect::<Vec<_>>())
            .finish()
    }
}

#[cfg(test)]
#[path = "secret_tests.rs"]
mod tests;
