//! Node CLI 的用户级配置 `~/.zcode/cli/config.json`（docs/specs/rust-cli/config.md §1）。
//!
//! 读：只负责读文件并判断是不是 JSON 对象；`permission`、`hooks`、`mcp` 各段由入口层交给各能力 crate
//! 的解析器。文件可以没有；读不出或不是对象时整份忽略（Node `config_file_invalid`）。
//! 写：界面语言 `ui.locale`（tui.md §7.1，Node `updateUiLocaleInFileConfig`）、主题 `ui.theme`
//! （tui-manage.md §2.1，同一种写法）与 `/mcp enable/disable` 的 `enabled`（tui-manage.md §6.1），都经
//! [`update_cli_config`] 这一个写者。

use std::io;

use rust_cli_fs::LockOptions;
use rust_cli_paths::{StoragePath, StoragePaths};
use serde_json::{Map, Value};

use crate::credentials::PRIVATE_WRITE;

/// 界面语言的配置值（Node `UiLocale`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiLocale {
    Auto,
    EnUs,
    ZhCn,
}

impl UiLocale {
    /// 只认 Node 的三个值（`isUiLocale`）。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "en-US" => Some(Self::EnUs),
            "zh-CN" => Some(Self::ZhCn),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::EnUs => "en-US",
            Self::ZhCn => "zh-CN",
        }
    }
}

/// 主题的配置值（Node `ui.theme`：`auto` / `dark` / `light`，docs/specs/rust-cli/tui-manage.md §2.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiTheme {
    Auto,
    Dark,
    Light,
}

impl UiTheme {
    /// 只认 Node schema 的三个值。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }
}

/// 写配置失败；文案与 Node `file-config.adapter.ts` 相同。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigWriteError {
    #[error("Unable to read config file: {0}")]
    Read(String),
    #[error("Unable to parse config file as JSON: {0}")]
    Parse(String),
    #[error("Config file must contain a JSON object: {0}")]
    NotObject(String),
    #[error("Unable to write config file: {0}")]
    Write(String),
    /// `/mcp enable/disable`：这个文件里没有这个服务器（tui-manage.md §6.1）。
    #[error("MCP server {0} is not defined in {1}")]
    NoMcpServer(String, String),
}

/// 读一份配置文件的结果。
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigFile {
    /// 文件不存在。
    Missing,
    /// 读不出、不是 JSON 或不是对象；`reason` 进警告。
    Invalid {
        reason: String,
    },
    Loaded(Map<String, Value>),
}

impl ConfigFile {
    /// 解析读到的字节（项目级文件经执行环境读到后也用它判断）。
    pub fn parse(bytes: &[u8]) -> Self {
        match serde_json::from_slice::<Value>(bytes) {
            Ok(Value::Object(object)) => Self::Loaded(object),
            Ok(_) => Self::Invalid {
                reason: "the file is not a JSON object".to_owned(),
            },
            Err(error) => Self::Invalid {
                reason: error.to_string(),
            },
        }
    }

    /// 读到的对象；没有或无效时为 None。
    pub fn object(&self) -> Option<&Map<String, Value>> {
        match self {
            Self::Loaded(object) => Some(object),
            Self::Missing | Self::Invalid { .. } => None,
        }
    }
}

/// 读用户级配置（每次都以磁盘为准，不缓存）。
pub async fn read_cli_config(paths: &StoragePaths) -> ConfigFile {
    let file = paths.cli_config();
    match rust_cli_fs::read_optional(file.as_path()).await {
        Ok(Some(bytes)) => ConfigFile::parse(&bytes),
        Ok(None) => ConfigFile::Missing,
        Err(error) => ConfigFile::Invalid {
            reason: error.to_string(),
        },
    }
}

/// 用户级配置的唯一写入口：持 Node 形状的目录锁（`config.json.lock/`，fs 的 `with_file_lock`）读、改、
/// 原子写（0600）。`edit` 收文件的字节（不存在时为 None）与错误里显示的路径，返回新的文件文字。返回写入的
/// 文件。
///
/// 缺陷修复 R2-12：`ui.locale`、`ui.theme` 与 MCP 的 `enabled` 原来各自读改写、不加锁；TUI 里这些写回并发跑
/// （`/theme` 紧接着 `/mcp disable`）时后写的覆盖先写的。现在都经这里，同一进程与多个进程之间都按锁串行。
/// 写出的文字不变（Node 互操作逐字节相同）；Node 写这个文件不加锁，锁只在本工程的写者之间起作用。
async fn update_cli_config(
    paths: &StoragePaths,
    edit: impl FnOnce(Option<&[u8]>, &str) -> Result<String, ConfigWriteError>,
) -> Result<StoragePath, ConfigWriteError> {
    let file = paths.cli_config();
    let path = file.as_path();
    let shown = path.display().to_string();
    let written = rust_cli_fs::with_file_lock(path, &LockOptions::default(), async || {
        let Ok(bytes) = rust_cli_fs::read_optional(path).await else {
            return Ok(Err(ConfigWriteError::Read(shown.clone())));
        };
        let text = match edit(bytes.as_deref(), &shown) {
            Ok(text) => text,
            Err(error) => return Ok(Err(error)),
        };
        let written = rust_cli_fs::atomic_write(path, text.as_bytes(), PRIVATE_WRITE).await;
        Ok::<_, io::Error>(written.map_err(|_| ConfigWriteError::Write(shown.clone())))
    })
    .await;
    match written {
        Ok(Ok(())) => Ok(file),
        Ok(Err(error)) => Err(error),
        // 拿不到锁（超时）或建不了目录：同写失败。
        Err(_) => Err(ConfigWriteError::Write(shown)),
    }
}

/// 把 `ui.locale` 写回用户级配置（Node `updateUiLocaleInFileConfig`，tui.md §7.1）：读改写，其余字段与
/// 键序不变；文件读不出或不是对象时报错、不写。返回写入的文件。
pub async fn write_ui_locale(
    paths: &StoragePaths,
    locale: UiLocale,
) -> Result<StoragePath, ConfigWriteError> {
    write_ui_value(paths, "locale", locale.as_str()).await
}

/// 把 `ui.theme` 写回用户级配置（tui-manage.md §2.1）：与 `ui.locale` 同一套写法。
pub async fn write_ui_theme(
    paths: &StoragePaths,
    theme: UiTheme,
) -> Result<StoragePath, ConfigWriteError> {
    write_ui_value(paths, "theme", theme.as_str()).await
}

async fn write_ui_value(
    paths: &StoragePaths,
    key: &str,
    value: &str,
) -> Result<StoragePath, ConfigWriteError> {
    update_cli_config(paths, |bytes, shown| {
        let mut object = match bytes.map(serde_json::from_slice::<Value>) {
            None => Map::new(),
            Some(Ok(Value::Object(object))) => object,
            Some(Ok(_)) => return Err(ConfigWriteError::NotObject(shown.to_owned())),
            Some(Err(_)) => return Err(ConfigWriteError::Parse(shown.to_owned())),
        };
        patch_ui(&mut object, key, value);
        let mut text = node_pretty(&Value::Object(object));
        text.push('\n');
        Ok(text)
    })
    .await
}

/// Node `patchUiLocale`：`{...parsed, ui: {...currentUi, <key>: 值}}`。已有的键保持位置（serde_json 按插入
/// 顺序），没有的加在末尾；`ui` 不是对象时换成只有这个键的对象。
fn patch_ui(object: &mut Map<String, Value>, key: &str, value: &str) {
    let ui = object
        .entry("ui")
        .or_insert_with(|| Value::Object(Map::new()));
    if !ui.is_object() {
        *ui = Value::Object(Map::new());
    }
    if let Value::Object(ui) = ui {
        ui.insert(key.to_owned(), Value::String(value.to_owned()));
    }
}

/// `/mcp enable/disable` 的写法（docs/specs/rust-cli/tui-manage.md §6.1，Node 的 `enabled` 字段）：
/// `mcp.servers.<名字>.enabled` 设为开关（已有的键原位，没有时加在末尾），去掉旧字段 `enable`；返回新的
/// 文件文字（两格缩进加换行）。`bytes` 为 None 表示文件不存在。`shown` 是错误里显示的路径。
pub fn mcp_enabled_text(
    bytes: Option<&[u8]>,
    shown: &str,
    name: &str,
    enabled: bool,
) -> Result<String, ConfigWriteError> {
    let missing = || ConfigWriteError::NoMcpServer(name.to_owned(), shown.to_owned());
    let mut object = match bytes.map(serde_json::from_slice::<Value>) {
        None => return Err(missing()),
        Some(Ok(Value::Object(object))) => object,
        Some(Ok(_)) => return Err(ConfigWriteError::NotObject(shown.to_owned())),
        Some(Err(_)) => return Err(ConfigWriteError::Parse(shown.to_owned())),
    };
    let server = object
        .get_mut("mcp")
        .and_then(|mcp| mcp.get_mut("servers"))
        .and_then(|servers| servers.get_mut(name))
        .and_then(Value::as_object_mut)
        .ok_or_else(missing)?;
    server.insert("enabled".to_owned(), Value::Bool(enabled));
    server.shift_remove("enable");
    let mut text = node_pretty(&Value::Object(object));
    text.push('\n');
    Ok(text)
}

/// 用户级配置里的 `/mcp enable/disable`（tui-manage.md §6.1）：经 [`update_cli_config`] 读改写，原子写，0600。
/// 返回写入的文件。
pub async fn write_mcp_enabled(
    paths: &StoragePaths,
    name: &str,
    enabled: bool,
) -> Result<StoragePath, ConfigWriteError> {
    update_cli_config(paths, |bytes, shown| {
        mcp_enabled_text(bytes, shown, name, enabled)
    })
    .await
}

/// `JSON.stringify(value, null, 2)`：先按 JS 规则得到紧凑文本（键序、数字写法），再缩进。
pub(crate) fn node_pretty(value: &Value) -> String {
    let compact = rust_cli_base::js::json::stringify(value);
    serde_json::from_str::<Value>(&compact)
        .and_then(|value| serde_json::to_string_pretty(&value))
        .unwrap_or(compact)
}

#[cfg(test)]
#[path = "cli_config_tests.rs"]
mod tests;
