//! PDF 的页数与取页（`Pdf::pdf_page_count` / `render_pdf_pages`；docs/specs/rust-cli/tools-files.md §1.5，
//! 修复线 F24）。
//!
//! 外部程序是 poppler-utils 的 `pdfinfo` 与 `pdftoppm`（命令见 tools-files.md §1.5）：文件内容经 stdin 交给它们
//! （参数是 `-`，Windows 的长路径与非 ASCII 路径因此不影响
//! poppler）。进程经 `rust-cli-proc` 启动，环境是白名单投影（同 git），超时或调用方丢下 future 时 `kill_on_drop`
//! 结束它。程序名由操作系统按 PATH 查找（Windows 上补 `.exe`）。端口只给原始结果，报错文字在内核的 tools 里拼。

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use rust_cli_kernel_api::env::EnvError;
use rust_cli_kernel_api::env_pdf::{PdfRender, RenderPdfPages};
use tokio::io::AsyncWriteExt;

use crate::helpers::run;
use crate::meta::io_error;

/// pdfinfo 的时限。
const INFO_TIMEOUT: Duration = Duration::from_secs(10);
/// `pdftoppm -v` 的时限。
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// 取页的时限。
const RENDER_TIMEOUT: Duration = Duration::from_secs(120);

/// 一次运行的结果；超时时 `code` 为 None。
struct Ran {
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// 跑一个外部程序。`input` 经 stdin 写入（程序不读就退出时写入失败，不算错）；超时时丢下子进程（kill_on_drop）。
/// 起不来（找不到程序等）时为 Err。
async fn execute(
    program: &str,
    args: &[OsString],
    input: Option<&[u8]>,
    env: &BTreeMap<String, String>,
    timeout: Duration,
) -> io::Result<Ran> {
    let mut command = rust_cli_proc::command(program);
    command
        .args(args)
        .env_clear()
        .envs(env)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let stdin = child.stdin.take();
    let feed = async move {
        if let (Some(mut stdin), Some(input)) = (stdin, input) {
            let _ = stdin.write_all(input).await;
        }
    };
    let finished = async { tokio::join!(feed, child.wait_with_output()).1 };
    match tokio::time::timeout(timeout, finished).await {
        Ok(output) => {
            let output = output?;
            Ok(Ran {
                code: output.status.code(),
                stdout: output.stdout,
                stderr: output.stderr,
            })
        }
        Err(_) => Ok(Ran {
            code: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }),
    }
}

async fn read_input(path: &Path) -> Result<Vec<u8>, EnvError> {
    let target = path.to_path_buf();
    run(move || crate::files::read_file(&target).map(|file| file.bytes)).await
}

/// `Pages:` 行（`/^Pages:\s+(\d+)/m`）。
fn pages_line(stdout: &str) -> Option<u64> {
    stdout.lines().find_map(|line| {
        let rest = line.strip_prefix("Pages:")?;
        let digits = rest.trim_start_matches(rust_cli_base::js::string::is_space);
        if digits.len() == rest.len() {
            return None;
        }
        let count = digits.len()
            - digits
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .len();
        digits[..count].parse().ok()
    })
}

/// `pdfinfo -`：退出码 0 且有 `Pages:` 一行时是页数；找不到程序、失败、超时时为 None。
pub(crate) async fn page_count(
    path: &Path,
    env: &BTreeMap<String, String>,
) -> Result<Option<u64>, EnvError> {
    let input = read_input(path).await?;
    let args = [OsString::from("-")];
    match execute("pdfinfo", &args, Some(&input), env, INFO_TIMEOUT).await {
        Ok(ran) if ran.code == Some(0) => Ok(pages_line(&String::from_utf8_lossy(&ran.stdout))),
        Ok(_) | Err(_) => Ok(None),
    }
}

/// pdftoppm 是否可用：可用的结论在实例内保留，不可用时每次重查。
#[derive(Debug, Default)]
pub(crate) struct Poppler {
    available: AtomicBool,
}

/// 输出目录名的序号（同一进程里不重复；别的进程留下的同名目录让创建失败，换下一个）。
static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

/// 本次取页的输出目录：`<scratch>/pdf/<pid>-<n>`。正常结束时 [`OutputDir::remove`]；调用方丢下 future 时在
/// Drop 里删（只有几张页面图）。
struct OutputDir {
    path: PathBuf,
    armed: bool,
}

#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是工作区与执行环境私有临时目录的文件 IO 入口（docs/specs/rust-cli/README.md §4）"
)]
fn create_unique(base: &Path) -> Result<PathBuf, EnvError> {
    std::fs::create_dir_all(base).map_err(|error| io_error(base, &error))?;
    loop {
        let name = format!(
            "{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        );
        let dir = base.join(name);
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io_error(&dir, &error)),
        }
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是执行环境私有临时目录的文件 IO 入口（docs/specs/rust-cli/README.md §4）"
)]
fn remove_tree(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

/// 输出目录里的 `.jpg`，按文件名排序（pdftoppm 按文档页数补零，排序即
/// 页序）读出。
#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是执行环境私有临时目录的文件 IO 入口（docs/specs/rust-cli/README.md §4）"
)]
fn read_pages(dir: &Path) -> Result<Vec<Vec<u8>>, EnvError> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map_err(|error| io_error(dir, &error))?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".jpg"))
        .collect();
    names.sort();
    names
        .iter()
        .map(|name| {
            let page = dir.join(name);
            std::fs::read(&page).map_err(|error| io_error(&page, &error))
        })
        .collect()
}

impl OutputDir {
    async fn create(scratch: &Path) -> Result<Self, EnvError> {
        rust_cli_proc::private::ensure(scratch)
            .await
            .map_err(|error| io_error(scratch, &error))?;
        let base = scratch.join("pdf");
        let path = run(move || create_unique(&base)).await?;
        Ok(Self { path, armed: true })
    }

    async fn remove(mut self) {
        self.armed = false;
        let path = self.path.clone();
        let _ = run(move || {
            remove_tree(&path);
            Ok(())
        })
        .await;
    }
}

impl Drop for OutputDir {
    fn drop(&mut self) {
        if self.armed {
            remove_tree(&self.path);
        }
    }
}

impl Poppler {
    async fn pdftoppm(&self, env: &BTreeMap<String, String>) -> bool {
        if self.available.load(Ordering::Relaxed) {
            return true;
        }
        let args = [OsString::from("-v")];
        // 退出码 0，或退出码不是 127 且 stderr 非空（老版本的 `-v` 退出码是 99）。
        let found = execute("pdftoppm", &args, None, env, PROBE_TIMEOUT)
            .await
            .is_ok_and(|ran| {
                ran.code == Some(0) || (ran.code != Some(127) && !ran.stderr.is_empty())
            });
        if found {
            self.available.store(true, Ordering::Relaxed);
        }
        found
    }

    /// `pdftoppm -jpeg -r 100 -f <首页> [-l <末页>] - <scratch>/pdf/<…>/page`。
    pub(crate) async fn render(
        &self,
        path: &Path,
        request: &RenderPdfPages,
        scratch: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<PdfRender, EnvError> {
        if !self.pdftoppm(env).await {
            return Ok(PdfRender::Unavailable);
        }
        let input = read_input(path).await?;
        let output = OutputDir::create(scratch).await?;
        let mut args: Vec<OsString> = ["-jpeg", "-r", "100", "-f"]
            .into_iter()
            .map(OsString::from)
            .collect();
        args.push(request.first_page.to_string().into());
        if let Some(last) = request.last_page {
            args.push("-l".into());
            args.push(last.to_string().into());
        }
        args.push("-".into());
        args.push(output.path.join("page").into_os_string());
        let rendered = match execute("pdftoppm", &args, Some(&input), env, RENDER_TIMEOUT).await {
            Ok(ran) if ran.code == Some(0) => {
                let dir = output.path.clone();
                run(move || read_pages(&dir))
                    .await
                    .map(|pages| PdfRender::Rendered { pages })
            }
            Ok(ran) => Ok(PdfRender::Failed {
                exit_code: ran.code,
                stderr: String::from_utf8_lossy(&ran.stderr).into_owned(),
            }),
            // 可用性检查之后又起不来（程序被删掉等）：按失败报，stderr 是启动的错误。
            Err(error) => Ok(PdfRender::Failed {
                exit_code: None,
                stderr: error.to_string(),
            }),
        };
        output.remove().await;
        rendered
    }
}
