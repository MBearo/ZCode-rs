//! 交互界面的输入（docs/specs/rust-cli/tui.md §2、§7.1；tui-kit.md §2.3）：入口为 `Mode::Tui` 收齐的事实（[`Inputs`]），
//! 与它们整理成界面服务时共用的小件：终端与语言的环境变量、`!` 命令的 shell 名、远程 shell 的判断、`-c` / `-r` 的启动
//! 会话、`/memory` 的文件、界面语言与偏好的写回、`theme.json` 的存储。界面的 `Options` 在 `tui.rs`。
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rust_cli_base::clock::SystemClock;
use rust_cli_base::names::env;
use rust_cli_base::names::{DOT_DIR, LOCAL_RULES_FILE, RULES_FILE};
use rust_cli_base::secret::EnvSnapshot;
use rust_cli_base::ui_prefs::UiLocale;
use rust_cli_fs::{FileMode, ParentDir, TempName, WriteOptions};
use rust_cli_headless::SessionTarget;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::{PermissionMode, RuleBehavior, RuleSource};
use rust_cli_kernel_api::provider::ModelDirectory;
use rust_cli_node_store::NodeIds;
use rust_cli_paths::StoragePaths;
use rust_cli_platform::local::Recorder;
use rust_cli_tui::Startup;
use rust_cli_tui_ports::Os;
use rust_cli_tui_ports::services::{
    LocaleStore, MemoryFile, Preferences, StoreFile, Stores, WorkspaceTrust,
};
use rust_cli_tui_ports::update::UpdateInfo;

#[cfg(test)]
#[path = "tui_inputs_tests.rs"]
mod tests;

/// 界面的机器服务与 MCP 服务器的机器标记（remote-exec.md §21.2、§22.8）。
pub(crate) type MachineInputs = (
    Option<Arc<dyn rust_cli_tui_ports::machines::Machines>>,
    Vec<(String, String)>,
);

/// 入口为 `Mode::Tui` 准备的事实。
pub(crate) struct Inputs<'a> {
    pub workspace: WorkspaceRef,
    pub cwd: &'a Path,
    pub home: &'a Path,
    pub env: &'a EnvSnapshot,
    /// 入口的临时目录（外部编辑器的私有目录在它下面，F58）。
    pub temp: &'a Path,
    /// 新会话的缺省模型：TUI 不等它定下就画第一帧（performance.md §12.3）。
    pub selection: rust_cli_kernel_api::default_model::DefaultModel,
    /// 进程启动的时刻（`startup.ready` 的起点，logging.md §4）。
    pub started: std::time::Instant,
    pub mode: PermissionMode,
    pub models: Option<Arc<dyn ModelDirectory>>,
    /// `-c` / `-r [id]` / 不带 id 的 `-r`（[`startup`]）。
    pub startup: Startup,
    /// `--fork-session`（cli-args.md）：启动打开的会话先在末尾 fork。
    pub fork: bool,
    /// 配置文件的 `ui.locale`（config.md §2）与存储位置（写回用）。
    pub locale: Option<UiLocale>,
    pub paths: StoragePaths,
    /// 平台层：界面的登录与退出（tui.md §7.2）。
    pub account: Arc<rust_cli_platform::Platform>,
    /// MCP 服务器的来源文件与写项目级文件的执行环境（tui-manage.md §6.1）。
    pub mcp: crate::frontends::tui_mcp::McpConfigFiles,
    /// 退出信号（session-lifecycle.md §3.5）。
    pub stop: tokio_util::sync::CancellationToken,
    /// 与内核同一份的时钟与 ID（README §4，F7）。
    pub clock: Arc<SystemClock>,
    pub ids: Arc<NodeIds>,
    /// 应用内切到别的项目时的信任判定（workspace-trust.md §2.8）。
    pub trust: Arc<dyn WorkspaceTrust>,
    /// npm 上有更新的版本时会话里的提示（update.md §7，F48）。
    pub update: Option<UpdateInfo>,
    /// 运行的平台（修复线 F65，tui-input.md §9）。
    pub os: Os,
    /// 配置的 `askUserQuestionTimeout`（毫秒，tools-more.md §1.1，工作线 F72）。
    pub question_timeout: Option<u64>,
    /// 内核的 `AutoCompactAt`（配置的 `compact.autoThresholdTokens`，设置页的 Auto-compact at 与 `/autocompact`，
    /// compact.md §2.3）：与压缩同一份；与用户级之后的配置层里定了这个键的文件。
    pub compact_at: (rust_cli_runtime::AutoCompactAt, Option<String>),
    /// 远端机器（remote-exec.md §21.2）：`/machines` 与机器状态行；绑定到远端机器的 MCP 服务器（服务器名，机器名；
    /// R5，§22.8）。
    pub machines: MachineInputs,
    /// 配置文件来源的规则来自的文件（来源、种类、路径）与 `--add-dir`：设置的 Permissions 段（tui-manage.md §1.2）。
    pub rule_files: Vec<(RuleSource, RuleBehavior, String)>,
    pub add_dirs: Vec<String>,
}

/// 界面用到的环境变量：色深、语言、终端种类、复用器、SSH、滚轮。
const VARS: &[&str] = &[
    env::TERM,
    env::COLORTERM,
    env::TERM_PROGRAM,
    env::TERM_PROGRAM_VERSION,
    env::KITTY_WINDOW_ID,
    env::WT_SESSION,
    env::LANG,
    env::LC_ALL,
    env::LC_MESSAGES,
    // 系统语言按 Node `detectLocale` 也看 LANGUAGE（tui.md §7.1）。
    env::LANGUAGE,
    // 终端通知在 tmux / screen 里要包一层（tui-manage.md §5.2）。
    env::TMUX,
    env::STY,
    // 复制的提示与 OSC 52（tui-select.md §3）：SSH、复用器、终端名。
    env::SSH_CONNECTION,
    env::ZELLIJ,
    env::LC_TERMINAL,
    env::KONSOLE_VERSION,
    env::GNOME_TERMINAL_SERVICE,
    env::XTERM_VERSION,
    env::VTE_VERSION,
    env::ALACRITTY_LOG,
    // 滚轮每格的行数（tui-select.md §4.2）；速度是 `ZCODE_SCROLL_SPEED`。
    env::CURSOR_TRACE_ID,
    env::VSCODE_GIT_ASKPASS_MAIN,
    env::TERMINAL_EMULATOR,
    env::ZCODE_SCROLL_SPEED,
];

/// 环境变量有非空的值。
pub(crate) fn has(env: &EnvSnapshot, name: &str) -> bool {
    env.raw(name).is_some()
}

pub(crate) fn vars(env: &EnvSnapshot) -> BTreeMap<String, String> {
    env.vars()
        .iter()
        .filter(|(key, _)| VARS.contains(&key.as_str()))
        .cloned()
        .collect()
}

/// `!` 命令跑在哪个 shell 里（Scala `Main.shellName`）：`$SHELL`，Windows 是 `ComSpec`；只要文件名，去掉 `.exe`。
pub(crate) fn shell_name(env: &EnvSnapshot) -> String {
    let path = if cfg!(windows) {
        env.get(env::ComSpec).unwrap_or("cmd.exe")
    } else {
        env.raw(env::SHELL).unwrap_or("/bin/sh")
    };
    let name = path.rsplit(['/', '\\']).next().unwrap_or("sh");
    match name.len().checked_sub(4) {
        Some(cut) if name.is_char_boundary(cut) && name[cut..].eq_ignore_ascii_case(".exe") => {
            name[..cut].to_owned()
        }
        _ => name.to_owned(),
    }
}

/// 在远程 shell 里（Scala `Place.from` 的 `remote`）：有 SSH，或 Linux 上没有图形界面。
pub(crate) fn remote_shell(env: &EnvSnapshot) -> bool {
    has(env, env::SSH_CONNECTION)
        || has(env, env::SSH_TTY)
        || (cfg!(target_os = "linux") && !has(env, env::DISPLAY) && !has(env, env::WAYLAND_DISPLAY))
}

/// `-c` / `-r [id]` / 不带 id 的 `-r` → 界面启动时打开的会话。
pub(crate) fn startup(target: &SessionTarget, pick: bool) -> Startup {
    match target {
        _ if pick => Startup::Pick,
        SessionTarget::New => Startup::New,
        SessionTarget::Continue => Startup::Latest,
        SessionTarget::Resume(id) => Startup::Session(id.as_str().to_owned()),
    }
}

/// `/memory` 的文件（tui-input.md §4.2）：用户级与项目级的规则文件。
pub(crate) fn memory_files(cwd: &Path, home: &Path) -> Vec<MemoryFile> {
    let file = |label: &str, display: String, path: PathBuf| MemoryFile {
        label: label.to_owned(),
        display,
        path: path.display().to_string(),
    };
    vec![
        file(
            "User memory",
            format!("~/{DOT_DIR}/{RULES_FILE}"),
            rust_cli_paths::user_path(home, "").join(RULES_FILE),
        ),
        file(
            "Project memory",
            format!("./{RULES_FILE}"),
            cwd.join(RULES_FILE),
        ),
        file(
            "Project memory (local)",
            format!("./{LOCAL_RULES_FILE}"),
            cwd.join(LOCAL_RULES_FILE),
        ),
    ]
}

/// 界面语言写回用户级配置文件（tui.md §7.1，platform `write_ui_locale`）。
pub(crate) struct ConfigLocale {
    pub(crate) paths: StoragePaths,
}

impl LocaleStore for ConfigLocale {
    fn save(&self, locale: UiLocale) -> BoxFuture<'static, Result<String, String>> {
        let paths = self.paths.clone();
        async move {
            rust_cli_platform::local::write_ui_locale(&paths, locale)
                .await
                .map(|file| file.as_path().display().to_string())
                .map_err(|error| error.to_string())
        }
        .boxed()
    }
}

/// `/effort` 缺省、`/mcp enable/disable`、Auto-compact at 的写回（tui-manage.md §2、§6.1）；主题写 `theme.json`
/// （[`stores`]）。
pub(crate) struct ConfigPreferences {
    pub(crate) paths: StoragePaths,
    pub(crate) platform: Arc<rust_cli_platform::Platform>,
    pub(crate) mcp: Arc<crate::frontends::tui_mcp::McpConfigFiles>,
}

impl Preferences for ConfigPreferences {
    fn save_default_model(
        &self,
        selection: ModelSelection,
    ) -> BoxFuture<'static, Result<String, String>> {
        let platform = self.platform.clone();
        async move { platform.save_default_model(&selection).await }.boxed()
    }

    fn save_mcp_enabled(
        &self,
        server: String,
        enabled: bool,
    ) -> BoxFuture<'static, Result<Option<String>, String>> {
        let (mcp, paths) = (self.mcp.clone(), self.paths.clone());
        async move { mcp.save(&paths, &server, enabled).await }.boxed()
    }

    fn save_compact_at(&self, at: Option<u64>) -> BoxFuture<'static, Result<String, String>> {
        let paths = self.paths.clone();
        async move {
            rust_cli_platform::local::write_compact_at(&paths, at)
                .await
                .map(|file| file.as_path().display().to_string())
                .map_err(|error| error.to_string())
        }
        .boxed()
    }
}

/// `theme.json` 等小文件在数据根的 `puffin-tui/` 下（与 Scala Puffin 的 `DataDir` 相同：`<数据根>/.zcode/puffin-tui`）。
/// 目录名是用户数据的位置，不随界面改名：改了，已经选过的主题就读不到了。
pub(crate) fn stores(paths: &StoragePaths) -> Option<Arc<dyn Stores>> {
    let dir = data_root(paths)?.join(STORES_DIR);
    Some(Arc::new(FileStores { dir }))
}

/// 小文件的目录名（见 [`stores`]）。
const STORES_DIR: &str = "puffin-tui";

/// 数据根（`<ZCODE_DATA_BASE_DIR 或 HOME>/.zcode`）：用户级配置文件所在的目录。
fn data_root(paths: &StoragePaths) -> Option<PathBuf> {
    paths
        .zcode_config()
        .as_path()
        .parent()
        .map(Path::to_path_buf)
}

/// 原子写（docs/puffin-tui.md §4.2：另一个 TUI 读到的要么是旧文件，要么是新文件）：与 Scala 的 `FolderFile` 一样用可见的
/// 临时名，父目录不存在时建好。
const WRITE: WriteOptions = WriteOptions {
    mode: FileMode::Default,
    temp_name: TempName::Visible,
    parent: ParentDir::Create,
    rename_retry: &[],
};

/// `theme.json`、`models.json`、`modes.json`（格式见 docs/puffin-tui.md §4.2）：都在 `dir` 下，与 Scala Puffin 共用。
pub(crate) struct FileStores {
    pub dir: PathBuf,
}

impl FileStores {
    pub(crate) fn path(&self, file: StoreFile) -> PathBuf {
        self.dir.join(match file {
            StoreFile::Theme => "theme.json",
            StoreFile::Models => "models.json",
            StoreFile::Modes => "modes.json",
            StoreFile::Cockpit => "cockpit.json",
        })
    }
}

impl Stores for FileStores {
    fn read(&self, file: StoreFile) -> BoxFuture<'static, Result<Option<String>, String>> {
        let path = self.path(file);
        async move {
            match rust_cli_fs::read_optional(&path).await {
                Ok(Some(bytes)) => String::from_utf8(bytes)
                    .map(Some)
                    .map_err(|_| format!("{} is not UTF-8", path.display())),
                Ok(None) => Ok(None),
                Err(error) => Err(format!("{}: {error}", path.display())),
            }
        }
        .boxed()
    }

    fn write(&self, file: StoreFile, text: String) -> BoxFuture<'static, Result<(), String>> {
        let path = self.path(file);
        async move {
            rust_cli_fs::atomic_write(&path, text.as_bytes(), WRITE)
                .await
                .map_err(|error| format!("{}: {error}", path.display()))
        }
        .boxed()
    }
}

/// 输入历史的追加（tui.md「输入历史」）：交给 platform `History` 唯一的写者。
pub(crate) struct HistoryWriter(pub(crate) Recorder);

impl rust_cli_tui::HistoryRecorder for HistoryWriter {
    fn record(&self, text: &str, at_ms: u64) {
        self.0.record(text, at_ms);
    }
}

/// 内核读的自动压缩位置（compact `AutoCompactAt`）交给界面：设置页与 `/autocompact` 改的是同一份（compact.md §2.3）。
/// 界面不依赖 runtime，只认 [`rust_cli_tui::AutoCompactCell`]。
pub(crate) struct CompactCell(pub(crate) rust_cli_runtime::AutoCompactAt);

impl rust_cli_tui::AutoCompactCell for CompactCell {
    fn get(&self) -> Option<u64> {
        self.0.get()
    }

    fn set(&self, at: Option<u64>) {
        self.0.set(at);
    }
}
