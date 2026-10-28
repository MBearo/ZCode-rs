//! 本机就是 macOS 时的真实 SSH 夹具（F107，docs/specs/rust-cli/remote-exec.md §28.10）：在 127.0.0.1 的一个高端口上起一个
//! 只给本测试用的 `sshd`（临时主机密钥、临时客户端密钥、`-F` 临时 ssh 配置、`SetEnv HOME=<临时目录>/home`），用完杀掉。
//!
//! 不改本机的任何系统设置（不开远程登录、不动防火墙）：`sshd` 以当前用户的身份跑在前台，只有这把临时密钥能登录，
//! 远端的 `$HOME` 是临时目录，所以不碰用户真实的 `~/.zcode`、`~/.ssh`。
//!
//! 临时目录放在 `/tmp` 下而不是 `$TMPDIR`：远端的 `executor.sock` 在 `$HOME/.zcode/rust-cli/exec/` 下，macOS 的
//! `sun_path` 上限是 104 字节，`$TMPDIR`（`/var/folders/…`）加上这一段会超。
#![allow(dead_code, reason = "不同的测试文件用到的辅助函数不同")]

use std::io::Write;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rust_cli_ssh_connect::SshTarget;

/// ssh 配置里的目标名。
pub const HOST: &str = "f107-mac";

/// 本测试自己的 sshd：进程、临时目录与 ssh 配置。丢掉时杀掉进程、删掉目录。
pub struct LocalSshd {
    child: Child,
    dir: PathBuf,
    pub port: u16,
    pub user: String,
}

impl Drop for LocalSshd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // 字面的绝对路径（自己建的临时目录）。按字符串比前缀：`Path::starts_with` 只认整段。
        if self
            .dir
            .to_string_lossy()
            .starts_with("/tmp/f107-zcode-rs-sshd-")
        {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// 没有 `/usr/sbin/sshd`、起不来或不是 macOS 时跳过（写明原因）。
pub fn sshd_or_skip() -> Option<LocalSshd> {
    if !cfg!(target_os = "macos") {
        eprintln!("macOS SSH test skipped: this test only runs on macOS");
        return None;
    }
    if !Path::new("/usr/sbin/sshd").is_file() {
        eprintln!("macOS SSH test skipped: /usr/sbin/sshd is not installed");
        return None;
    }
    match LocalSshd::start() {
        Ok(sshd) => Some(sshd),
        Err(reason) => {
            eprintln!("macOS SSH test skipped: {reason}");
            None
        }
    }
}

impl LocalSshd {
    fn start() -> Result<Self, String> {
        let user = whoami()?;
        let dir = PathBuf::from(format!(
            "/tmp/f107-zcode-rs-sshd-{}-{:x}",
            std::process::id(),
            rust_cli_base::random::u64() & 0xffff
        ));
        std::fs::create_dir_all(dir.join("home")).map_err(|error| error.to_string())?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
        for key in ["id_ed25519", "ssh_host_ed25519_key"] {
            let output = Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-C", "zcode-test", "-f"])
                .arg(dir.join(key))
                .output()
                .map_err(|error| format!("ssh-keygen: {error}"))?;
            if !output.status.success() {
                return Err(format!(
                    "ssh-keygen: {}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
        }
        std::fs::copy(dir.join("id_ed25519.pub"), dir.join("authorized_keys"))
            .map_err(|error| error.to_string())?;
        std::fs::set_permissions(
            dir.join("authorized_keys"),
            std::fs::Permissions::from_mode(0o600),
        )
        .map_err(|error| error.to_string())?;
        let port = free_port()?;
        let config = format!(
            "Port {port}\n\
             ListenAddress 127.0.0.1\n\
             HostKey {dir}/ssh_host_ed25519_key\n\
             AuthorizedKeysFile {dir}/authorized_keys\n\
             PidFile {dir}/sshd.pid\n\
             StrictModes no\n\
             UsePAM no\n\
             PasswordAuthentication no\n\
             KbdInteractiveAuthentication no\n\
             PubkeyAuthentication yes\n\
             PermitUserEnvironment no\n\
             AllowTcpForwarding no\n\
             X11Forwarding no\n\
             UseDNS no\n\
             SetEnv HOME={dir}/home\n\
             LogLevel ERROR\n",
            dir = dir.display()
        );
        std::fs::write(dir.join("sshd_config"), config).map_err(|error| error.to_string())?;
        let child = Command::new("/usr/sbin/sshd")
            .arg("-f")
            .arg(dir.join("sshd_config"))
            .args(["-D", "-e"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("sshd: {error}"))?;
        let sshd = Self {
            child,
            dir,
            port,
            user,
        };
        let host_key = std::fs::read_to_string(sshd.path("ssh_host_ed25519_key.pub"))
            .map_err(|error| error.to_string())?;
        let mut fields = host_key.split_whitespace();
        let (kind, key) = (
            fields.next().unwrap_or_default(),
            fields.next().unwrap_or_default(),
        );
        std::fs::write(
            sshd.path("known_hosts"),
            format!("[127.0.0.1]:{port} {kind} {key}\n"),
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(
            sshd.path("ssh_config"),
            format!(
                "Host {HOST}\n  HostName 127.0.0.1\n  Port {port}\n  User {}\n  IdentityFile {}\n  \
                 IdentitiesOnly yes\n  IdentityAgent none\n  UserKnownHostsFile {}\n  \
                 GlobalKnownHostsFile /dev/null\n  StrictHostKeyChecking yes\n  LogLevel ERROR\n",
                sshd.user,
                sshd.path("id_ed25519").display(),
                sshd.path("known_hosts").display()
            ),
        )
        .map_err(|error| error.to_string())?;
        sshd.wait_ready()?;
        Ok(sshd)
    }

    /// 等 sshd 能登录（最多 10 s）。
    fn wait_ready(&self) -> Result<(), String> {
        let mut last = String::new();
        for _ in 0..100 {
            let (ok, _, err) = self.ssh("true");
            if ok {
                return Ok(());
            }
            last = err;
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(format!("the test sshd did not accept a login: {last}"))
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// 远端的 `$HOME`（sshd 的 `SetEnv` 给的临时目录）。
    pub fn home(&self) -> PathBuf {
        self.dir.join("home")
    }

    pub fn config(&self) -> String {
        self.path("ssh_config").to_string_lossy().into_owned()
    }

    /// ssh 进程的环境：临时 HOME，没有 ssh-agent。
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            (
                "HOME".to_owned(),
                self.path("ssh-home").to_string_lossy().into_owned(),
            ),
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
        ]
    }

    pub fn target(&self) -> SshTarget {
        SshTarget {
            destination: HOST.to_owned(),
            program: PathBuf::from("ssh"),
            config: Some(self.path("ssh_config")),
            options: Vec::new(),
            env: self.env(),
        }
    }

    /// 经 ssh 在“远端”（就是本机，另一个 HOME）跑一条命令。
    pub fn ssh(&self, command: &str) -> (bool, String, String) {
        let output = Command::new("ssh")
            .args([
                "-F",
                &self.config(),
                "-T",
                "-o",
                "BatchMode=yes",
                HOST,
                command,
            ])
            .env_clear()
            .envs(self.env())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

fn whoami() -> Result<String, String> {
    let output = Command::new("/usr/bin/id")
        .arg("-un")
        .output()
        .map_err(|error| format!("id -un: {error}"))?;
    let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if name.is_empty() {
        return Err("id -un printed nothing".to_owned());
    }
    Ok(name)
}

/// 一个没人用的端口（让系统分配，马上放掉；sshd 随后绑它）。
fn free_port() -> Result<u16, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    let port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    drop(listener);
    Ok(port)
}

/// 照 npm 平台包的样子打包（`package/bin/zcode-rs` 0755、`package/build.json`），返回 tarball 的字节。
pub fn pack(binary: &Path, target: &str, version: &str, dir: &Path) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let package = dir.join("package");
    std::fs::create_dir_all(package.join("bin")).unwrap();
    let copied = package.join("bin/zcode-rs");
    std::fs::copy(binary, &copied).unwrap();
    std::fs::set_permissions(&copied, std::fs::Permissions::from_mode(0o755)).unwrap();
    let bytes = std::fs::read(&copied).unwrap();
    let sha256: String = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let build = serde_json::json!({
        "version": version,
        "sourceCommit": "0".repeat(40),
        "target": target,
        "sha256": sha256,
    });
    std::fs::write(package.join("build.json"), build.to_string()).unwrap();
    std::fs::write(package.join("package.json"), "{}").unwrap();
    let tgz = dir.join("package.tgz");
    let output = Command::new("tar")
        .args(["--format", "ustar", "-czf"])
        .arg(&tgz)
        .arg("-C")
        .arg(dir)
        .arg("package")
        .env("COPYFILE_DISABLE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "tar: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tarball = std::fs::read(&tgz).unwrap();
    // 打包用的副本不留（执行器有一百多 MB）。
    let _ = std::fs::remove_dir_all(&package);
    let _ = std::fs::remove_file(&tgz);
    tarball
}

/// 假注册表的路由：路径 → 回复的字节。
type Routes = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

/// 本地的假 npm 注册表：按路径回复字节（没有的路径 404），记下请求的路径。
pub struct Registry {
    pub url: String,
    routes: Routes,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Registry {
    pub fn start() -> Self {
        use std::io::{BufRead, BufReader};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let routes: Routes = Arc::default();
        let (log, table) = (seen.clone(), routes.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                if reader.read_line(&mut first).unwrap_or(0) == 0 {
                    continue;
                }
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                }
                let path = first.split_whitespace().nth(1).unwrap_or("").to_owned();
                log.lock().unwrap().push(path.clone());
                let (status, body) = table
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(route, _)| *route == path)
                    .map_or((404, b"{}".to_vec()), |(_, body)| (200, body.clone()));
                let head = format!(
                    "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        Self { url, routes, seen }
    }

    pub fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    /// 发布一个平台包的某个版本（packument 与 tarball 两条路由；latest 就是它）。返回（packument 的路径，tarball 的路径）。
    pub fn publish(&self, id: &str, version: &str, tarball: Vec<u8>) -> (String, String) {
        use base64::Engine;
        use sha2::{Digest, Sha512};
        let name = format!("@mbearo/zcode-{id}");
        let packument = format!("/@mbearo%2fzcode-{id}");
        let tarball_path = format!("/{name}/-/zcode-{id}-{version}.tgz");
        let integrity = format!(
            "sha512-{}",
            base64::engine::general_purpose::STANDARD.encode(Sha512::digest(&tarball))
        );
        let document = serde_json::json!({
            "name": name,
            "dist-tags": {"latest": version},
            "versions": {version: {"name": name, "version": version, "dist": {
                "tarball": format!("{}{tarball_path}", self.url),
                "integrity": integrity,
            }}},
        });
        self.routes.lock().unwrap().extend([
            (packument.clone(), document.to_string().into_bytes()),
            (tarball_path.clone(), tarball),
        ]);
        (packument, tarball_path)
    }
}
