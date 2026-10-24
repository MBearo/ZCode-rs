//! 机器清单（docs/specs/rust-cli/remote-exec.md §20.3.1）：名字、ssh 目标、工作目录与“上次是否连着”；不存密钥。
//!
//! 文件不存在是空清单；格式不对时报错（调用方记 warn、按空清单），不改文件。不认识的字段（以后加的）读时留着、写回时
//! 照写。写经 fs 的原子写（私有权限）；每个操作先读再写，没有内存里的副本（文件是唯一事实源）。

use std::path::{Path, PathBuf};

use rust_cli_fs::{FileMode, ParentDir, TempName, WriteOptions};
use rust_cli_paths::StoragePath;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// 本机的名字：不能用作远端机器名。
const LOCAL: &str = "local";
/// 名字的长度上限。
const MAX_NAME: usize = 32;
/// 文件格式的版本。
const VERSION: u64 = 1;

const WRITE: WriteOptions = WriteOptions {
    mode: FileMode::Private,
    temp_name: TempName::Hidden,
    parent: ParentDir::Create,
    rename_retry: &[],
};

#[derive(Debug, thiserror::Error)]
pub enum MachinesError {
    #[error("cannot read {path}: {message}")]
    Read { path: String, message: String },
    #[error("cannot write {path}: {message}")]
    Write { path: String, message: String },
    #[error("{path} is not a valid machine list: {message}")]
    Invalid { path: String, message: String },
    #[error(
        "invalid machine name {0:?}: use 1-32 letters, digits, '.', '_' or '-', and not \"local\""
    )]
    Name(String),
    #[error("a machine named {0:?} already exists")]
    Exists(String),
    #[error("there is no machine named {0:?}")]
    Missing(String),
}

/// 清单里的一台机器。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineEntry {
    pub name: String,
    /// ssh 目标（`~/.ssh/config` 的别名或 `user@host`）。
    pub ssh: String,
    /// 那台机器上的工作目录；没写时是远端的 home。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    /// 用户最后一次的选择是连着：下次启动自动连。断线与退出不改它。
    #[serde(default)]
    pub connected: bool,
    /// 不认识的字段：写回时照写。
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

impl MachineEntry {
    pub fn new(name: &str, ssh: &str, working_directory: Option<&str>) -> Self {
        Self {
            name: name.to_owned(),
            ssh: ssh.to_owned(),
            working_directory: working_directory.map(str::to_owned),
            connected: false,
            rest: Map::new(),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    version: u64,
    #[serde(default)]
    machines: Vec<MachineEntry>,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

/// 机器名是否可用。
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name != LOCAL
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// `machines.json` 的读写。
#[derive(Debug, Clone)]
pub struct MachineList {
    path: PathBuf,
}

impl MachineList {
    pub fn new(path: &StoragePath) -> Self {
        Self::at(path.as_path())
    }

    /// 测试与示例：直接给路径。
    pub fn at(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
        }
    }

    fn shown(&self) -> String {
        self.path.display().to_string()
    }

    async fn read(&self) -> Result<File, MachinesError> {
        let bytes = rust_cli_fs::read_optional(&self.path)
            .await
            .map_err(|error| MachinesError::Read {
                path: self.shown(),
                message: error.to_string(),
            })?;
        let Some(bytes) = bytes else {
            return Ok(File::default());
        };
        let file: File =
            serde_json::from_slice(&bytes).map_err(|error| MachinesError::Invalid {
                path: self.shown(),
                message: error.to_string(),
            })?;
        if let Some(bad) = file.machines.iter().find(|m| !valid_name(&m.name)) {
            return Err(MachinesError::Invalid {
                path: self.shown(),
                message: format!("invalid machine name {:?}", bad.name),
            });
        }
        Ok(file)
    }

    async fn write(&self, mut file: File) -> Result<(), MachinesError> {
        file.version = VERSION;
        let mut text =
            serde_json::to_string_pretty(&file).map_err(|error| MachinesError::Write {
                path: self.shown(),
                message: error.to_string(),
            })?;
        text.push('\n');
        rust_cli_fs::atomic_write(&self.path, text.as_bytes(), WRITE)
            .await
            .map_err(|error| MachinesError::Write {
                path: self.shown(),
                message: error.to_string(),
            })
    }

    /// 全部机器，按文件里的顺序。
    pub async fn load(&self) -> Result<Vec<MachineEntry>, MachinesError> {
        Ok(self.read().await?.machines)
    }

    /// 加一台机器（`/machines add`，R4）。
    pub async fn add(&self, machine: MachineEntry) -> Result<(), MachinesError> {
        if !valid_name(&machine.name) {
            return Err(MachinesError::Name(machine.name));
        }
        let mut file = self.read().await?;
        if file.machines.iter().any(|m| m.name == machine.name) {
            return Err(MachinesError::Exists(machine.name));
        }
        file.machines.push(machine);
        self.write(file).await
    }

    /// 删掉一台机器。
    pub async fn remove(&self, name: &str) -> Result<(), MachinesError> {
        let mut file = self.read().await?;
        let before = file.machines.len();
        file.machines.retain(|m| m.name != name);
        if file.machines.len() == before {
            return Err(MachinesError::Missing(name.to_owned()));
        }
        self.write(file).await
    }

    /// 记下用户的选择（连上为真、用户断开为假）。值没变时不写文件。
    pub async fn set_connected(&self, name: &str, connected: bool) -> Result<(), MachinesError> {
        let mut file = self.read().await?;
        let Some(machine) = file.machines.iter_mut().find(|m| m.name == name) else {
            return Err(MachinesError::Missing(name.to_owned()));
        };
        if machine.connected == connected {
            return Ok(());
        }
        machine.connected = connected;
        self.write(file).await
    }
}

#[cfg(test)]
#[path = "list_tests.rs"]
mod tests;
