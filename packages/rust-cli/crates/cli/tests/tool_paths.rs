//! 工具路径参数的规范化（F14，docs/specs/rust-cli/tools-files.md §3.2）：经执行管线调用真实的文件工具
//! （env-local 外面套一层序列化往返），核对工具路径参数的三条规则——去首尾空白、NFC、
//! 解析后以空白结尾时拒绝。修前路径原样使用：带空白的路径被当成工作区里的相对路径，分解形式的文件名与
//! readFileState 里记的合成形式对不上。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、读回写出的文件名"
)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_env_local::LocalEnvironment;
use rust_cli_kernel_api::env::{EnvPath, OperationContext, Origin, WorkspaceRef};
use rust_cli_kernel_api::ids::{
    ArtifactId, EnvironmentId, Generation, InteractionId, OperationId, OwnerEpoch, RunId,
    SessionId, ToolUseId,
};
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::orchestration::{
    InteractionAnswer, InteractionRequest, Interactions, OrchestrationError, PermissionGate,
    ToolExecutor, ToolRegistry,
};
use rust_cli_kernel_api::permission::{PermissionDecision, PermissionMode};
use rust_cli_kernel_api::store::{ArtifactPurpose, ArtifactRef, ArtifactStore, StoreError};
use rust_cli_kernel_api::testing::{NoHooks, RoundTripEnv, local_services, machine};
use rust_cli_kernel_api::tool::{Tool, ToolUseContext};
use rust_cli_tools::{BuiltinTools, Pipeline};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// 权限不在这里测（policy `files_tests.rs`）：一律放行。
struct AllowAll;

#[async_trait]
impl PermissionGate for AllowAll {
    async fn decide(
        &self,
        _tool: &dyn Tool,
        _input: &Value,
        _context: &ToolUseContext,
    ) -> PermissionDecision {
        PermissionDecision::Allow {
            updated_input: None,
        }
    }
}

struct NoQuestions;

#[async_trait]
impl Interactions for NoQuestions {
    async fn ask(
        &self,
        _session: &SessionId,
        _request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        Ok(InteractionAnswer {
            interaction: InteractionId::new("i"),
            answer: json!({"decision": "allow"}),
        })
    }
}

#[derive(Default)]
struct Artifacts(Mutex<HashMap<String, Vec<u8>>>);

#[async_trait]
impl ArtifactStore for Artifacts {
    async fn put(
        &self,
        session: &SessionId,
        purpose: ArtifactPurpose,
        media_type: String,
        bytes: Vec<u8>,
    ) -> Result<ArtifactRef, StoreError> {
        let mut stored = self.0.lock().unwrap();
        let id = format!("art_{}", stored.len() + 1);
        let size = bytes.len() as u64;
        stored.insert(id.clone(), bytes);
        Ok(ArtifactRef {
            session: session.clone(),
            id: ArtifactId::new(id),
            purpose,
            media_type,
            size,
        })
    }

    async fn get(&self, artifact: &ArtifactRef) -> Result<Vec<u8>, StoreError> {
        self.0
            .lock()
            .unwrap()
            .get(artifact.id.as_str())
            .cloned()
            .ok_or(StoreError::Storage {
                message: "missing".to_owned(),
            })
    }

    /// Read 读落盘位置时先按会话与 id 找回引用（修复线 F18，tools-files.md §1.4）；大小取自存的内容。
    async fn find(
        &self,
        session: &SessionId,
        id: &ArtifactId,
    ) -> Result<Option<ArtifactRef>, StoreError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .get(id.as_str())
            .map(|bytes| ArtifactRef {
                session: session.clone(),
                id: id.clone(),
                purpose: ArtifactPurpose::ToolOutput,
                media_type: "text/plain".to_owned(),
                size: bytes.len() as u64,
            }))
    }
}

/// 一个工作区与一套工具（readFileState 在工具里，同一个会话的各步共用）。
struct Bench {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    tools: BuiltinTools,
    pipeline: Pipeline,
    env: Arc<RoundTripEnv<LocalEnvironment>>,
    artifacts: Arc<Artifacts>,
    step: usize,
}

impl Bench {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // Windows 上 canonicalize 给 `\\?\` 前缀，下面 `path` 用 `/` 拼名字会成为无效路径（F66），所以不规范化。
        let root = if cfg!(windows) {
            dir.path().to_path_buf()
        } else {
            std::fs::canonicalize(dir.path()).unwrap()
        };
        let gate: Arc<dyn PermissionGate> = Arc::new(AllowAll);
        Self {
            _dir: dir,
            root,
            tools: rust_cli_tools::builtin(gate.clone()).with_search_opt_in(true),
            pipeline: Pipeline::new(gate, Arc::new(NoHooks)),
            env: Arc::new(RoundTripEnv(Arc::new(LocalEnvironment::new(None)))),
            artifacts: Arc::new(Artifacts::default()),
            step: 0,
        }
    }

    fn path(&self, name: &str) -> String {
        format!("{}/{name}", self.root.display())
    }

    /// 跑一次工具，返回 tool_result 的文本与 is_error。
    async fn call(&mut self, tool: &str, input: Value) -> (String, bool) {
        self.step += 1;
        let session = SessionId::new("00000000-0000-4000-8000-000000000001");
        let env = self.env.clone();
        let context = ToolUseContext {
            session: session.clone(),
            run: RunId::new("run_1"),
            tool_use: ToolUseId::new(format!("toolu_{:03}", self.step)),
            agent: None,
            workspace: WorkspaceRef {
                environment: EnvironmentId::new("local"),
                root: EnvPath::new(self.root.to_string_lossy()),
            },
            mode: PermissionMode::DontAsk,
            cancel: CancellationToken::new(),
            machine: machine(env.clone()),
            local: local_services(env),
            artifacts: self.artifacts.clone(),
            interactions: Arc::new(NoQuestions),
            operation: OperationContext {
                environment: EnvironmentId::new("local"),
                generation: Generation::new("g1"),
                owner_epoch: OwnerEpoch::LOCAL,
                operation: OperationId::new(format!("op_{}", self.step)),
                origin: Origin::Agent {
                    session,
                    run: RunId::new("run_1"),
                },
            },
        };
        let tool = self.tools.find(tool).unwrap();
        let (progress, _updates) = mpsc::channel(4);
        let output = self.pipeline.execute(tool, input, context, progress).await;
        let text = output
            .content
            .iter()
            .filter_map(|block| match block {
                ToolResultContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        (text, output.is_error)
    }
}

/// 目录里的文件名（原样的字节，看得出合成与分解形式）。
fn names(root: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn read_glob_and_grep_trim_white_space_around_the_path() {
    let mut bench = Bench::new();
    std::fs::write(bench.root.join("a.txt"), "alpha\n").unwrap();
    let padded = format!(" {}\n", bench.path("a.txt"));
    let (text, error) = bench.call("Read", json!({ "file_path": padded })).await;
    assert!(!error && text.contains("1\talpha"), "{text}");
    let padded_root = format!("\t{} ", bench.root.display());
    let (text, error) = bench
        .call("Glob", json!({ "pattern": "*.txt", "path": padded_root }))
        .await;
    assert!(!error && text.contains("a.txt"), "{text}");
    let (text, error) = bench
        .call(
            "Grep",
            json!({ "pattern": "alpha", "path": padded_root, "output_mode": "content" }),
        )
        .await;
    assert!(!error && text.contains("alpha"), "{text}");
}

#[tokio::test]
async fn paths_are_nfc_so_read_state_matches_across_spellings() {
    let mut bench = Bench::new();
    let composed = "caf\u{e9}.txt";
    let decomposed = "cafe\u{301}.txt";
    // Write 用分解形式：磁盘上的名字是合成形式（结果做 NFC）。
    let (text, error) = bench
        .call(
            "Write",
            json!({ "file_path": bench.path(decomposed), "content": "one\n" }),
        )
        .await;
    assert!(!error, "{text}");
    assert_eq!(names(&bench.root), vec![composed.to_owned()]);
    // 用分解形式读、用合成形式改：readFileState 的键相同，Edit 不报“没读过”。
    let (text, error) = bench
        .call("Read", json!({ "file_path": bench.path(decomposed) }))
        .await;
    assert!(!error && text.contains("1\tone"), "{text}");
    let (text, error) = bench
        .call(
            "Edit",
            json!({
                "file_path": bench.path(composed),
                "old_string": "one",
                "new_string": "two",
            }),
        )
        .await;
    assert!(!error, "{text}");
    assert_eq!(
        std::fs::read_to_string(bench.root.join(composed)).unwrap(),
        "two\n"
    );
}

/// Windows 不能建以空格结尾的目录名（系统会去掉末尾的空格），只在 Unix 上跑。
#[cfg(unix)]
#[tokio::test]
async fn a_path_that_ends_in_white_space_once_resolved_is_refused() {
    let mut bench = Bench::new();
    std::fs::create_dir(bench.root.join("x ")).unwrap();
    std::fs::write(bench.root.join("x ").join("n.ipynb"), "{}").unwrap();
    for (tool, field, input) in [
        (
            "Read",
            "file_path",
            json!({ "file_path": bench.path("x /.") }),
        ),
        (
            "Write",
            "file_path",
            json!({ "file_path": bench.path("x /."), "content": "c" }),
        ),
        (
            "Edit",
            "file_path",
            json!({ "file_path": bench.path("x /."), "old_string": "a", "new_string": "a" }),
        ),
        (
            "NotebookEdit",
            "notebook_path",
            json!({ "notebook_path": bench.path("x /."), "new_source": "s" }),
        ),
    ] {
        let (text, error) = bench.call(tool, input).await;
        let expected = format!(
            "{tool} {field} ends in white space once its \".\" and \"..\" are resolved, and {tool} refuses such a path. If the white space is not meant, send the path without it. {tool} cannot open a file whose own name ends in white space."
        );
        assert!(error && text.contains(&expected), "{tool}: {text}");
    }
    // 末尾是分隔符时判为稳定（Node `normalize` 保留它），照常读：`x ` 是目录。
    let (text, error) = bench
        .call("Read", json!({ "file_path": bench.path("x /") }))
        .await;
    assert!(error && !text.contains("ends in white space"), "{text}");
}

/// 落盘的大结果（`zcode-artifact://<会话>/<artifact>`，tools-files.md D5）不是执行环境的路径：Read 的校验与读取
/// 都不经路径规范化，地址原样交给 ArtifactStore（以 `.`、空白结尾的 ID 也不按“解析后以空白结尾”拒绝）。
#[tokio::test]
async fn artifact_addresses_are_not_normalized_as_paths() {
    let mut bench = Bench::new();
    let session = SessionId::new("00000000-0000-4000-8000-000000000001");
    for id in ["art_1", "art_2 /."] {
        bench
            .artifacts
            .0
            .lock()
            .unwrap()
            .insert(id.to_owned(), b"first\nsecond\n".to_vec());
        let address = format!("zcode-artifact://{session}/{id}");
        let (text, error) = bench
            .call("Read", json!({ "file_path": address, "limit": 1 }))
            .await;
        assert!(!error && text == "1\tfirst", "{id}: {text}");
    }
}
