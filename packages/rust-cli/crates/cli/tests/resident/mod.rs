//! 常驻执行器的真实进程测试共用的夹具（docs/specs/rust-cli/remote-exec.md §19.12、§22.10）：临时的用户环境（socket 与
//! 账本都在里面，丢掉时停掉它的常驻执行器）、每次连接拉起一个 `zcode executor --stdio` 代理的连接器。
#![allow(dead_code, reason = "不同的测试文件用到的辅助函数不同")]

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rust_cli_exec_client::{ClientOptions, ExecClient, Reconnect};
use rust_cli_exec_transport::stream::stream_link;
use rust_cli_exec_transport::{Connector, Link, LinkError, MAX_FRAME};
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use serde_json::Value;

pub const BIN: &str = env!("CARGO_BIN_EXE_zcode-rs");

/// 一个临时的用户环境；丢掉时停掉它的常驻执行器。
pub struct Home {
    home: tempfile::TempDir,
    pub work: PathBuf,
    _work: tempfile::TempDir,
}

impl Home {
    pub fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        Self {
            work: std::fs::canonicalize(work.path()).unwrap(),
            home,
            _work: work,
        }
    }

    pub fn env(&self) -> Vec<(String, String)> {
        let home = self.home.path().to_string_lossy().into_owned();
        vec![
            ("HOME".to_owned(), home.clone()),
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
            ("SHELL".to_owned(), "/bin/bash".to_owned()),
            ("TMPDIR".to_owned(), home),
        ]
    }

    pub fn command(&self, args: &[&str]) -> std::process::Command {
        let mut command = std::process::Command::new(BIN);
        command
            .args(args)
            .env_clear()
            .envs(self.env())
            .current_dir(&self.work);
        command
    }

    pub fn run(&self, args: &[&str]) -> (i32, String, String) {
        let output = self.command(args).output().unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    pub fn status(&self) -> Value {
        let (_, stdout, _) = self.run(&["executor", "status", "--json"]);
        serde_json::from_str(stdout.trim()).unwrap()
    }

    /// 这个用户环境的 HOME。
    pub fn home(&self) -> &std::path::Path {
        self.home.path()
    }

    pub fn exec_dir(&self) -> PathBuf {
        self.home.path().join(".zcode/rust-cli/exec")
    }

    pub fn workspace(&self) -> WorkspaceRef {
        WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new(self.work.to_string_lossy()),
        }
    }

    /// 等常驻进程的状态满足条件（测试里的等待；产品代码不轮询）。
    pub async fn wait_for(&self, what: &str, check: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..400 {
            let status = self.status();
            if check(&status) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("timed out waiting for {what}: {}", self.status());
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.command(&["executor", "stop"]).output();
    }
}

/// 每次连接拉起一个 `zcode executor --stdio` 代理，记下它的 pid（测试要杀它）。
pub struct Proxies {
    home_env: Vec<(String, String)>,
    cwd: PathBuf,
    pids: Mutex<Vec<u32>>,
    children: Mutex<Vec<tokio::process::Child>>,
}

impl Proxies {
    pub fn new(home: &Home) -> Arc<Self> {
        Arc::new(Self {
            home_env: home.env(),
            cwd: home.work.clone(),
            pids: Mutex::default(),
            children: Mutex::default(),
        })
    }
    pub fn last(&self) -> u32 {
        *self.pids.lock().unwrap().last().unwrap()
    }

    pub fn count(&self) -> usize {
        self.pids.lock().unwrap().len()
    }
}

#[async_trait]
impl Connector for Proxies {
    async fn connect(&self) -> Result<Box<dyn Link>, LinkError> {
        let mut child = tokio::process::Command::new(BIN)
            .args(["executor", "--stdio"])
            .env_clear()
            .envs(self.home_env.clone())
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| LinkError::Connect(error.to_string()))?;
        self.pids.lock().unwrap().push(child.id().unwrap());
        let link = stream_link(
            child.stdout.take().unwrap(),
            child.stdin.take().unwrap(),
            "proxy",
            MAX_FRAME,
        );
        self.children.lock().unwrap().push(child);
        Ok(Box::new(link))
    }

    fn describe(&self) -> String {
        "test proxy".to_owned()
    }
}

pub async fn client(proxies: &Arc<Proxies>, instance: &str) -> ExecClient {
    let mut options = ClientOptions::new("test", instance);
    options.reconnect = Reconnect {
        attempts: Some(20),
        delay: Duration::from_millis(100),
        max_delay: Duration::from_millis(100),
    };
    ExecClient::connect(proxies.clone(), options).await.unwrap()
}

pub fn kill(pid: u32) {
    let status = std::process::Command::new("/bin/kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success());
}
