//! 插件读 Node 的配置与已安装插件的真实进程（docs/specs/rust-cli/ext.md §4）：临时 home 里的用户级配置
//! （`plugins.dirs`、`enabledPlugins`）、`installed_plugins.json`、官方内置插件的缓存与 bundled 分片，本地
//! 假的 Anthropic 端点。启用的插件提供 skill 与 hook；未启用的已安装插件与依赖 Node 运行时的内置插件
//! （browser-use）都不加载。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、检查写出的文件"
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex};

const REPLY: &str = concat!(
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",",
    "\"role\":\"assistant\",\"model\":\"m\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\n",
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
);

/// 每个连接回同一段 SSE 文本回复，并记下请求体。
fn model_server() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let recorded = bodies.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            recorded
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&body).into_owned());
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                REPLY.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(REPLY.as_bytes());
        }
    });
    (format!("http://{address}"), bodies)
}

fn zcode(home: &Path, work: &Path, args: &[&str], server: &str) -> Output {
    let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
    command
        .args(args)
        .current_dir(work)
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", home)
        .env("ZCODE_MODEL_BASE_URL", server)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", "claude-opus-5-5")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.output().unwrap()
}

fn write(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// 一个插件：manifest、一个 skill、一个在 UserPromptSubmit 时写标记文件的 hook。
fn plugin(dir: &Path, name: &str, skill: &str, marker: &str) {
    write(
        &dir.join(".zcode-plugin/plugin.json"),
        &format!(r#"{{"name": "{name}", "version": "1.0.0"}}"#),
    );
    write(
        &dir.join(format!("skills/{skill}/SKILL.md")),
        &format!("---\ndescription: The {skill} skill\n---\nBody\n"),
    );
    let hook = serde_json::json!({"hooks": {"UserPromptSubmit": [{"hooks": [
        {"type": "command", "command": format!("touch \"$ZCODE_PROJECT_DIR/{marker}\"")}
    ]}]}});
    write(&dir.join("hooks/hooks.json"), &hook.to_string());
}

#[test]
fn plugins_come_from_the_node_config_and_installed_plugins() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    let storage = home.join(".zcode/cli/plugins");

    // 配置 `plugins.dirs` 的插件缺省启用；已安装插件要 `enabledPlugins` 打开。
    plugin(&base.join("cfgkit"), "cfg-kit", "cfg-hello", "cfg.marker");
    plugin(
        &base.join("installed/kit"),
        "kit",
        "kit-skill",
        "kit.marker",
    );
    plugin(
        &base.join("installed/off"),
        "off",
        "off-skill",
        "off.marker",
    );
    let record = |name: &str| {
        serde_json::json!({
            "id": format!("{name}@market"), "name": name, "marketplace": "market", "version": "1.0.0",
            "installPath": base.join("installed").join(name).to_string_lossy(),
            "installedAt": "2026-10-01T00:00:00.000Z", "scope": "user"
        })
    };
    write(
        &storage.join("installed_plugins.json"),
        &serde_json::json!({"version": 1, "plugins": [record("kit"), record("off")]}).to_string(),
    );
    // 官方内置缓存：skill-creator 在缺省启用名单里；browser-use 依赖 Node 宿主的 node_repl，不加载。
    let cache = storage.join("cache/zcode-plugins-official");
    let creator = cache.join("skill-creator/0.1.0");
    let browser = cache.join("browser-use/0.5.1");
    plugin(&creator, "skill-creator", "skill-creator", "creator.marker");
    plugin(&browser, "browser-use", "control-browser", "browser.marker");
    let entry = |name: &str, path: &Path| serde_json::json!({"name": name, "cachePath": path.to_string_lossy(), "source": "filesystem"});
    write(
        &storage.join("marketplaces/zcode-plugins-official/bundled-marketplace.json"),
        &serde_json::json!({"version": 1, "manifest": {
            "name": "zcode-plugins-official",
            "plugins": [entry("skill-creator", &creator), entry("browser-use", &browser)]
        }})
        .to_string(),
    );
    write(
        &home.join(".zcode/cli/config.json"),
        &serde_json::json!({"plugins": {
            "dirs": [base.join("cfgkit").to_string_lossy()],
            "enabledPlugins": {"kit@market": true}
        }})
        .to_string(),
    );
    let (server, bodies) = model_server();

    let output = zcode(&home, &work, &["-p", "hello"], &server);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "done");
    for marker in ["cfg.marker", "kit.marker", "creator.marker"] {
        assert!(work.join(marker).exists(), "{marker} is missing: {stderr}");
    }
    for marker in ["off.marker", "browser.marker"] {
        assert!(!work.join(marker).exists(), "{marker} should not exist");
    }
    let request = bodies.lock().unwrap().first().cloned().unwrap();
    for skill in [
        "cfg-kit:cfg-hello",
        "kit:kit-skill",
        "skill-creator:skill-creator",
    ] {
        assert!(request.contains(skill), "{skill} is not listed");
    }
    for skill in ["off:off-skill", "browser-use:control-browser"] {
        assert!(!request.contains(skill), "{skill} should not be listed");
    }
}
