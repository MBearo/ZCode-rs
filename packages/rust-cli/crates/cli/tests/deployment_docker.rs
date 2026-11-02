//! 部署配置的 Docker 端到端（docs/specs/rust-cli/deployment.md §19.1，M1）：controller 在本机，会话的主 env 是 sshd
//! 容器里的执行器（deployment.md §5 的 ②）。真实的 `zcode -p` 只经部署配置（`ZCODE_DEPLOYMENT`）选主 env，模型是本机
//! 的脚本服务。断言 controller 要的东西都来自容器：
//!
//! 1. 工具：不写 `machine` 的 Bash 在容器里、在主 env 的工作目录跑；相对路径的 Read 读容器里的文件；
//! 2. 环境段：平台、工作目录是容器的；
//! 3. git 状态：容器里仓库的分支与提交（git 已移进主 env 的端口）；
//! 4. 规则：容器里项目的 AGENTS.md；
//! 5. 自动记忆：目录在容器的 home 下、已经建好；controller 的 home 下没有记忆目录；
//! 6. hook：定义在 controller 的用户级配置里（deployment.md §8），命令在容器里、在主 env 的工作目录跑；
//! 7. 命令行的路径参数指主 env（deployment.md §21）：相对路径的 `--append-system-prompt-file` 与 `--add-dir` 按容器里的
//!    工作目录解析、在容器里读和校验（controller 的 cwd 下没有这些路径）；
//! 8. root 检查看执行侧（deployment.md §21，Q10）：controller 是普通用户，容器里是 root，`bypassPermissions` 被拒；
//!    主流程用 `--allowedTools` 预先放行。
//!
//! 产物与跳过的规则同 `ssh_docker.rs`（`ZCODE_TEST_SSH_ARTIFACT` / `ZCODE_TEST_SSH_SKIP`）。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::print_stderr,
    clippy::unwrap_used,
    reason = "集成测试：起 Docker 容器、写部署配置、拉起 zcode 进程"
)]

mod sse;
mod sshd;

use std::process::{Command, Stdio};

use rust_cli_base::models::GLM_5;
use serde_json::{Value, json};
use sshd::{BIN, HOST, Image, Sshd, VERSION, artifact_or_skip};

/// 容器里的项目：git 仓库（分支 trunk）、项目规则、一个文件；旁边是命令行要指的附加目录与提示文件。
const PROJECT: &str = "set -e; mkdir -p /srv/proj /srv/shared /srv/notes \
    && printf 'Appended from the container.\\n' > /srv/notes/prompt.md && cd /srv/proj \
    && git init -q -b trunk && git config user.name sandbox-user && git config user.email dev@example.com \
    && printf 'Rule from the container.\\n' > AGENTS.md \
    && printf 'from the container\\n' > notes.txt \
    && git add -A && git commit -q -m 'container commit'";

/// 最后一条 user 消息（工具结果在里面）。合入 GLM 按 opus 处理之后，工具轮末尾另有一条 `<total_tokens>` 的 system
/// 提醒，所以不能取最后一条消息。
fn last_message(request: &Value) -> String {
    request["messages"]
        .as_array()
        .and_then(|messages| {
            messages
                .iter()
                .rev()
                .find(|message| message["role"] == "user")
        })
        .map(Value::to_string)
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_primary_env_is_the_container() {
    let Some(artifact) = artifact_or_skip() else {
        return;
    };
    let sshd = Sshd::start_with(Image::AlpineGit, "zcode-rs-test-deploy");
    sshd.wait_ready().await;
    let (ok, _, err) = sshd.ssh(PROJECT);
    assert!(ok, "{err}");

    // controller 这边：临时 home，cwd 是一个空目录（本机上没有这个项目）。
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let deployment = home.path().join("deployment.json");
    let config = json!({"primary_env": {"ssh": {
        "destination": HOST,
        "workdir": "/srv/proj",
        "ssh_config": sshd.config(),
        "artifact": artifact,
    }}});
    std::fs::write(&deployment, config.to_string()).unwrap();
    // 用户级配置（Node 的形状，config.md §2）留在 controller 本机；hook 命令把自己跑在哪写进容器的 /tmp。
    std::fs::create_dir_all(home.path().join(".zcode/cli")).unwrap();
    let hooks = json!({"hooks": {"enabled": true, "events": {"UserPromptSubmit": [{"hooks": [
        {"type": "command", "command": "echo \"hook in $(uname -s) at $(pwd)\" > /tmp/hook-ran"}
    ]}]}}});
    std::fs::write(
        home.path().join(".zcode/cli/config.json"),
        hooks.to_string(),
    )
    .unwrap();
    let bash = json!({
        "command": "uname -s; pwd; test -f /etc/alpine-release && echo inside-alpine",
        "description": "Where am I"
    })
    .to_string();
    let read = json!({"file_path": "notes.txt"}).to_string();
    let fixture = sse::serve(vec![
        sse::tools_reply(&[sse::tool_block(0, "toolu_1", "Bash", &bash)]),
        sse::tools_reply(&[sse::tool_block(0, "toolu_2", "Read", &read)]),
        sse::text_reply("done"),
    ]);
    let zcode = |args: &[&str]| {
        Command::new(BIN)
            .args(args)
            .current_dir(cwd.path())
            .env_clear()
            .env("HOME", home.path())
            .env("PATH", "/usr/bin:/bin")
            .env("SHELL", "/bin/bash")
            .env("ZCODE_DATA_BASE_DIR", home.path())
            .env("ZCODE_MODEL_BASE_URL", &fixture.url)
            .env("ZCODE_API_KEY", "test-key")
            .env("ZCODE_MODEL", GLM_5)
            .env("ZCODE_RUNTIME_ENV", "test")
            .env("ZCODE_DEPLOYMENT", &deployment)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    // 执行侧是 root：bypassPermissions 被拒（连上主 env 之后才检查；以前看的是 controller 这个普通用户，会放行）。
    let refused = zcode(&["-p", "anything", "--permission-mode", "bypassPermissions"]);
    assert_eq!(refused.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("cannot be used with root/sudo privileges"),
        "{stderr}"
    );
    assert!(fixture.main_requests().is_empty());
    let output = zcode(&[
        "-p",
        "check the sandbox",
        "--allowedTools",
        "Bash,Read",
        "--append-system-prompt-file",
        "../notes/prompt.md",
        "--add-dir",
        "../shared",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = fixture.main_requests();
    assert_eq!(requests.len(), 3);

    // 第一个请求：环境段、git 状态、规则与记忆段都是容器的。
    let first = requests[0].to_string();
    assert!(first.contains("Platform: linux"), "{first}");
    assert!(first.contains("/srv/proj"), "{first}");
    assert!(first.contains("Current branch: trunk"), "{first}");
    assert!(first.contains("container commit"), "{first}");
    assert!(first.contains("Rule from the container."), "{first}");
    // 命令行的路径参数：相对容器里的工作目录解析、在容器里读。
    assert!(first.contains("Appended from the container."), "{first}");
    assert!(
        first.contains("Additional working directories:\\n  - /srv/shared"),
        "{first}"
    );
    assert!(
        first.contains("/root/.zcode/projects/-srv-proj/memory/"),
        "{first}"
    );
    let (ok, _, err) = sshd.ssh("test -d /root/.zcode/projects/-srv-proj/memory");
    assert!(ok, "the memory directory is in the container: {err}");
    assert!(
        !home.path().join(".zcode/projects").exists(),
        "no memory directory on the controller"
    );

    // 工具：Bash 在容器里、在主 env 的工作目录跑；相对路径的 Read 读容器里的文件。
    let ran = last_message(&requests[1]);
    assert!(ran.contains("Linux"), "{ran}");
    assert!(ran.contains("/srv/proj"), "{ran}");
    assert!(ran.contains("inside-alpine"), "{ran}");
    let read = last_message(&requests[2]);
    assert!(read.contains("from the container"), "{read}");

    // hook：命令在容器里、在主 env 的工作目录跑。
    let (ok, out, err) = sshd.ssh("cat /tmp/hook-ran");
    assert!(ok, "the hook ran in the container: {err}");
    assert_eq!(out.trim(), "hook in Linux at /srv/proj");

    // 收尾：停掉容器里的常驻执行器（容器随后删掉）。
    let installed = format!("/root/.zcode/rust-cli/exec/bin/{VERSION}/zcode-rs");
    let (ok, out, err) = sshd.ssh(&format!("{installed} executor stop"));
    assert!(ok, "{out}{err}");
}
