//! 网络出口的真实进程（docs/specs/rust-cli/config.md §2.4、§5，修复线 F21）：本地 HTTP CONNECT 代理与本地
//! 自签名的 HTTPS 模型端点，跑 `zcode -p`，看请求走哪个代理、信任哪份 CA，以及格式不对时的行为。不依赖外网。
//!
//! 修复（审查 R2-2）：以前生产的出口是 `NetworkPolicy::default`，不读任何代理与 CA。旧代码上：经代理的几项
//! 都直连、证书不受信任而失败；格式不对的代理变量被忽略、照常运行；读不出的 `NODE_EXTRA_CA_CERTS` 没有警告。
#![allow(
    clippy::disallowed_methods,
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地代理与 HTTPS 夹具、跑真实的 zcode"
)]

mod system_env;
use rust_cli_base::models::GLM_5_3;
use system_env::KeepSystem as _;
mod egress;
mod sse;

use std::path::PathBuf;
use std::process::{Output, Stdio};

use egress::{ConnectProxy, TlsModel};

/// 临时 home 与工作区（`work`，带 `.git`）。
struct Sandbox {
    _dir: tempfile::TempDir,
    home: PathBuf,
    work: PathBuf,
}

fn sandbox() -> Sandbox {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    Sandbox {
        _dir: dir,
        home,
        work,
    }
}

impl Sandbox {
    /// 在工作区里写一个文件，返回绝对路径。
    fn write(&self, name: &str, text: &str) -> String {
        let path = self.work.join(name);
        std::fs::write(&path, text).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// 在工作区里跑一次 `zcode -p`：空环境，只给 home、模型端点与 `vars`。
    fn run(&self, base_url: &str, vars: &[(&str, &str)]) -> Output {
        let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
        command
            .args(["-p", "say done"])
            .current_dir(&self.work)
            .env_clear()
            .keep_system()
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("PATH", "/usr/bin:/bin")
            .env("TZ", "UTC")
            .env("ZCODE_DATA_BASE_DIR", &self.home)
            .env("ZCODE_MODEL_BASE_URL", base_url)
            .env("ZCODE_API_KEY", "test-key-network-egress")
            .env("ZCODE_MODEL", GLM_5_3);
        for (name, value) in vars {
            command.env(name, value);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap()
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// 跑成功，模型的回复到了 stdout。
fn succeeded(output: &Output) {
    assert!(
        output.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        text(&output.stdout),
        text(&output.stderr)
    );
    assert!(
        text(&output.stdout).contains("done"),
        "{}",
        text(&output.stdout)
    );
}

/// 代理收到的连接都是去模型端点的 CONNECT，至少一个。
fn tunnelled(proxy: &ConnectProxy, model: &TlsModel) {
    let target = format!("127.0.0.1:{}", model.port);
    let targets = proxy.targets();
    assert!(
        !targets.is_empty() && targets.iter().all(|seen| *seen == target),
        "{targets:?}"
    );
    assert!(!model.requests().is_empty());
}

#[test]
fn zcode_variables_win_over_the_standard_ones() {
    let agent = egress::pki("agent");
    let other = egress::pki("other");
    let model = egress::tls_model(&agent, sse::text_reply("done"));
    let zcode_proxy = egress::connect_proxy();
    let standard_proxy = egress::connect_proxy();
    let s = sandbox();
    s.write("agent-ca.pem", &agent.ca_pem);
    let other_ca = s.write("other-ca.pem", &other.ca_pem);
    let output = s.run(
        &model.url(),
        &[
            // Node 的写法：没有 scheme 时补 `http://`。
            (
                "ZCODE_HTTP_PROXY",
                &format!("127.0.0.1:{}", zcode_proxy.port),
            ),
            // 相对路径相对 cwd。
            ("ZCODE_AGENT_CA_CERT", "agent-ca.pem"),
            // 两套同时设：标准变量不生效。`NODE_EXTRA_CA_CERTS` 是无关的 CA，要是也被用上（或者
            // `ZCODE_AGENT_CA_CERT` 没被用上），模型端点的证书就不受信任。
            ("HTTPS_PROXY", &standard_proxy.url()),
            ("NODE_EXTRA_CA_CERTS", &other_ca),
        ],
    );
    succeeded(&output);
    tunnelled(&zcode_proxy, &model);
    assert_eq!(standard_proxy.targets(), Vec::<String>::new());
}

#[test]
fn standard_variables_route_through_the_proxy_and_add_the_ca() {
    let pki = egress::pki("extra");
    let model = egress::tls_model(&pki, sse::text_reply("done"));
    let lower = egress::connect_proxy();
    let upper = egress::connect_proxy();
    let s = sandbox();
    let ca = s.write("extra-ca.pem", &pki.ca_pem);
    let output = s.run(
        &model.url(),
        &[
            // 顺序：https_proxy → HTTPS_PROXY → http_proxy → HTTP_PROXY。
            ("https_proxy", &lower.url()),
            ("HTTPS_PROXY", &upper.url()),
            ("NODE_EXTRA_CA_CERTS", &ca),
        ],
    );
    succeeded(&output);
    tunnelled(&lower, &model);
    assert_eq!(upper.targets(), Vec::<String>::new());
}

#[test]
fn a_no_proxy_match_goes_direct() {
    let pki = egress::pki("direct");
    let model = egress::tls_model(&pki, sse::text_reply("done"));
    let proxy = egress::connect_proxy();
    let s = sandbox();
    let ca = s.write("ca.pem", &pki.ca_pem);
    // 规则：逗号或空白分隔，IP 按字面比较。
    succeeded(&s.run(
        &model.url(),
        &[
            ("HTTPS_PROXY", &proxy.url()),
            ("NO_PROXY", "example.test 127.0.0.1"),
            ("NODE_EXTRA_CA_CERTS", &ca),
        ],
    ));
    // Node 的规则：逗号分隔，带端口的项要端口相同。
    let rule = format!("example.test, 127.0.0.1:{}", model.port);
    succeeded(&s.run(
        &model.url(),
        &[
            ("ZCODE_HTTP_PROXY", &proxy.url()),
            ("ZCODE_NO_PROXY", &rule),
            ("ZCODE_AGENT_CA_CERT", &ca),
        ],
    ));
    assert_eq!(proxy.targets(), Vec::<String>::new());
    assert!(model.requests().len() >= 2, "{:?}", model.requests());
}

#[test]
fn an_invalid_standard_proxy_stops_before_any_request() {
    let fixture = sse::serve(Vec::new());
    let s = sandbox();
    let output = s.run(&fixture.url, &[("HTTPS_PROXY", "proxy.example.com:8080")]);
    // 固定的报错文字，退出码 1。
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stdout));
    assert_eq!(
        text(&output.stderr),
        concat!(
            "Invalid proxy URL in HTTPS_PROXY: \"proxy.example.com:8080\" cannot be parsed as a URL.\n",
            "Proxy settings must be a complete URL including the scheme, e.g. \"http://proxy.example.com:8080\".\n",
            "Fix or unset HTTPS_PROXY and restart ZCode.\n",
        )
    );
    assert_eq!(text(&output.stdout), "");
    assert!(fixture.main_requests().is_empty());

    // `ZCODE_HTTP_PROXY` 有效时不读标准变量，格式不对也不报错。
    let proxy = egress::connect_proxy();
    succeeded(&s.run(
        &fixture.url,
        &[
            ("ZCODE_HTTP_PROXY", &proxy.url()),
            ("ZCODE_NO_PROXY", "127.0.0.1"),
            ("HTTPS_PROXY", "proxy.example.com:8080"),
        ],
    ));
}

#[test]
fn ca_files_that_cannot_be_used() {
    let pki = egress::pki("unusable");
    let model = egress::tls_model(&pki, sse::text_reply("done"));
    let fixture = sse::serve(Vec::new());
    let s = sandbox();
    let missing = s.work.join("missing.pem");
    let missing = missing.to_string_lossy();

    // `ZCODE_AGENT_CA_CERT` 读不出：启动就报错退出（config.md D14），不发请求。
    let output = s.run(&fixture.url, &[("ZCODE_AGENT_CA_CERT", &missing)]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("ZCODE_AGENT_CA_CERT") && stderr.contains(missing.as_ref()),
        "{stderr}"
    );
    assert!(fixture.main_requests().is_empty());

    // 没有可用的证书：照常启动，HTTPS 在证书校验时失败（Node 的 `ca` 相同），请求没有到达。
    s.write("junk.pem", "not a certificate\n");
    let output = s.run(&model.url(), &[("ZCODE_AGENT_CA_CERT", "junk.pem")]);
    assert!(!output.status.success(), "{}", text(&output.stdout));
    assert_eq!(model.requests(), Vec::<String>::new());

    // `NODE_EXTRA_CA_CERTS` 读不出：写一行警告，照常运行。
    let output = s.run(&fixture.url, &[("NODE_EXTRA_CA_CERTS", &missing)]);
    succeeded(&output);
    assert!(
        text(&output.stderr).contains(&format!(
            "warn: ignoring extra certs from {missing}, load failed: No such file or directory\n"
        )),
        "{}",
        text(&output.stderr)
    );
}
