//! 模型轨迹文件（docs/specs/rust-cli/model-io.md §3.2、§3.3）：文件名、轮转、容量与消息压缩，逐条照 Node
//! `apps/zcode-cli/packages/adapters/src/model/runner-debug.ts`（两边共用目录与名额）。文件 IO 只经 `rust_cli_fs`；
//! 文件在不在、多大每次从磁盘读（另一个进程的轮转可能删掉本方的文件），压缩状态只在内存里。

use std::collections::HashMap;
use std::hash::Hasher;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rust_cli_base::units;
use rust_cli_fs::{FileMode, ParentDir, TempName, WriteOptions};
use serde_json::{Map, Value, json};

use crate::model_io::Pending;

const FILE_PREFIX: &str = "model-io-";
const FILE_SUFFIX: &str = ".jsonl";
/// 没有会话时的文件名段（Node `"no-session"`）。
const NO_SESSION: &str = "no-session";
/// 文件名段的上限（Node `sanitizeFileSegment` 的 `slice(0, 80)`）。
const SEGMENT_LIMIT: usize = 80;
/// 生产模式最多保留的文件数（Node `MAX_ROLLOUT_FILES`）。
const MAX_FILES: usize = 3;
/// 单文件上限（Node `MAX_ROLLOUT_SESSION_BYTES` / `MAX_DEBUG_SESSION_BYTES`）。
const PRODUCTION_CAP: u64 = 64 * units::MIB as u64;
const DEVELOPMENT_CAP: u64 = 256 * units::MIB as u64;
/// 写基线时最多保留的消息数（Node `MAX_ROLLOUT_BASELINE_MESSAGES` / `MAX_DEBUG_BASELINE_MESSAGES`）。
const PRODUCTION_BASELINE: usize = 64;
const DEVELOPMENT_BASELINE: usize = 256;
/// 到上限覆盖时的原因（Node `modelIOReset.reason`）。
const RESET_REASON: &str = "session_file_size_limit";
/// 覆盖整个文件：临时文件用可见名（以 `.tmp` 结尾，Node 的轮转只认 `model-io-*.jsonl`，不会把它算进名额）。
const OVERWRITE: WriteOptions = WriteOptions {
    mode: FileMode::Default,
    temp_name: TempName::Visible,
    parent: ParentDir::Create,
    rename_retry: &[],
};

/// 会话 → 文件名（Node `sanitizeFileSegment(sessionId) || "no-session"`）：连续的非 `[A-Za-z0-9_-]` 字符换成一个
/// `-`，去掉首尾的 `-`，截到 80 个字符。
pub(crate) fn file_name(session: Option<&str>) -> String {
    let mut segment = String::new();
    let mut in_run = false;
    for char in session.unwrap_or_default().chars() {
        if char.is_ascii_alphanumeric() || char == '_' || char == '-' {
            segment.push(char);
            in_run = false;
        } else if !in_run {
            segment.push('-');
            in_run = true;
        }
    }
    let mut segment: String = segment
        .trim_matches('-')
        .chars()
        .take(SEGMENT_LIMIT)
        .collect();
    if segment.is_empty() {
        segment = NO_SESSION.to_owned();
    }
    format!("{FILE_PREFIX}{segment}{FILE_SUFFIX}")
}

/// 一个文件上一条记录的请求消息（逐条哈希）与开始时刻：判断下一条能不能只写新增的消息。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct State {
    hashes: Vec<u64>,
    started_ms: u64,
}

/// 模型轨迹目录的唯一写者。
pub(crate) struct Files {
    dir: PathBuf,
    development: bool,
    states: HashMap<PathBuf, State>,
    warned: bool,
}

impl Files {
    pub(crate) fn new(dir: PathBuf, development: bool) -> Self {
        Self {
            dir,
            development,
            states: HashMap::new(),
            warned: false,
        }
    }

    /// 写一条（model-io.md §3.3 的步骤）。失败只吞掉；同一个进程第一次失败时记一行 warn。
    pub(crate) async fn write(&mut self, pending: Pending, full: bool, dropped: u64) {
        let path = self.dir.join(file_name(pending.session.as_deref()));
        if let Err(error) = self.write_one(&path, pending, full, dropped).await
            && !self.warned
        {
            self.warned = true;
            tracing::warn!(
                event = "model_io.write.failed",
                error = %error,
                "cannot write the model-io file"
            );
        }
    }

    async fn write_one(
        &mut self,
        path: &Path,
        pending: Pending,
        full: bool,
        dropped: u64,
    ) -> io::Result<()> {
        rust_cli_fs::create_dir_all(&self.dir).await?;
        if full {
            // 全量保留：不轮转、不重置、不压缩；仍更新压缩状态，关掉后可以接着只写新增的消息。
            let previous = None;
            let (line, state) = prepare(pending, previous, None, None, dropped).await?;
            rust_cli_fs::append(path, line.as_bytes()).await?;
            self.states.insert(path.to_path_buf(), state);
            return Ok(());
        }
        let existing = rust_cli_fs::file_len(path).await?;
        if !self.development && existing.is_none() {
            self.rotate(MAX_FILES - 1).await;
        }
        let (cap, baseline) = if self.development {
            (DEVELOPMENT_CAP, DEVELOPMENT_BASELINE)
        } else {
            (PRODUCTION_CAP, PRODUCTION_BASELINE)
        };
        let size = existing.unwrap_or(0);
        let reset = (size >= cap).then_some((cap, size));
        let previous = match (existing, reset) {
            (Some(_), None) => self.states.get(path).cloned(),
            _ => None,
        };
        let (line, state) = prepare(pending, previous, Some(baseline), reset, dropped).await?;
        if reset.is_some() {
            rust_cli_fs::atomic_write(path, line.as_bytes(), OVERWRITE).await?;
        } else {
            rust_cli_fs::append(path, line.as_bytes()).await?;
        }
        self.states.insert(path.to_path_buf(), state);
        Ok(())
    }

    /// 让目录里的 `model-io-*.jsonl` 不多于 `keep` 个：按修改时间从旧到新删（读不到时间的算最旧），删除失败的跳过
    /// （Node `rotateModelIOFiles`）。
    async fn rotate(&mut self, keep: usize) {
        let Ok(names) = rust_cli_fs::file_names(&self.dir).await else {
            return;
        };
        let names: Vec<String> = names
            .into_iter()
            .filter(|name| name.starts_with(FILE_PREFIX) && name.ends_with(FILE_SUFFIX))
            .collect();
        let mut extra = names.len().saturating_sub(keep);
        if extra == 0 {
            return;
        }
        let mut aged = Vec::with_capacity(names.len());
        for name in names {
            let path = self.dir.join(&name);
            let modified = rust_cli_fs::file_modified(&path)
                .await
                .ok()
                .flatten()
                .unwrap_or(UNIX_EPOCH);
            aged.push((modified, path));
        }
        aged.sort_by_key(|(modified, _)| *modified);
        for (_, path) in aged {
            if extra == 0 {
                break;
            }
            if rust_cli_fs::remove_optional(&path).await.is_ok() {
                self.states.remove(&path);
                extra -= 1;
            }
        }
    }
}

/// 在阻塞线程上拼一行：解析与脱敏（model 的 `into_json`）、压缩、`modelIOReset`、`droppedRecords`。
async fn prepare(
    pending: Pending,
    previous: Option<State>,
    baseline: Option<usize>,
    reset: Option<(u64, u64)>,
    dropped: u64,
) -> io::Result<(String, State)> {
    rust_cli_base::blocking(move || line(pending, previous.as_ref(), baseline, reset, dropped))
        .await
        .map_err(io::Error::other)
}

/// 一行记录（以换行结尾）与这个文件新的压缩状态。`baseline` 为 None 是全量保留（不压缩）。
pub(crate) fn line(
    pending: Pending,
    previous: Option<&State>,
    baseline: Option<usize>,
    reset: Option<(u64, u64)>,
    dropped: u64,
) -> (String, State) {
    let started_ms = pending.started_ms;
    let mut record = (pending.build)();
    let failed = record.contains_key("error");
    let state = compact(&mut record, previous, baseline, failed, started_ms);
    if let Some((cap, size)) = reset {
        record.insert(
            "modelIOReset".into(),
            json!({"maxFileBytes": cap, "previousFileBytes": size, "reason": RESET_REASON}),
        );
    }
    if dropped > 0 {
        record.insert("droppedRecords".into(), dropped.into());
    }
    let mut text = Value::Object(record).to_string();
    text.push('\n');
    (text, state)
}

/// 压缩 `request.body.messages`（Node `compactMessageCollection`，元数据写在 `request` 上）。
fn compact(
    record: &mut Map<String, Value>,
    previous: Option<&State>,
    baseline: Option<usize>,
    failed: bool,
    started_ms: u64,
) -> State {
    let mut state = State {
        hashes: Vec::new(),
        started_ms,
    };
    let Some(request) = record.get_mut("request").and_then(Value::as_object_mut) else {
        return state;
    };
    let Some(messages) = request
        .get_mut("body")
        .and_then(|body| body.get_mut("messages"))
        .and_then(Value::as_array_mut)
    else {
        return state;
    };
    state.hashes = messages.iter().map(hash).collect();
    let Some(max) = baseline else {
        return state;
    };
    let count = messages.len();
    let (kind, offset) = if failed {
        // 失败的请求要完整的请求体排查（Node `preserveFullBodyMessages`）。
        ("full", 0)
    } else if let Some(prefix) =
        previous.and_then(|previous| delta_base(previous, &state, count.min(max)))
    {
        messages.drain(..prefix);
        ("delta", prefix)
    } else if count > max {
        let offset = count - max;
        messages.drain(..offset);
        ("tail", offset)
    } else {
        ("full", 0)
    };
    request.insert("bodyMessageCount".into(), count.into());
    request.insert("bodyMessagesKind".into(), kind.into());
    request.insert("bodyMessageOffset".into(), offset.into());
    state
}

/// 只写新增消息时的起点（model-io.md §3.3）：两条从头开始相同的消息条数 L（最长公共前缀）。L ≥ 1、要写的条数
/// 不多于基线（`baseline` 条）、且上一条开始得不比这一条晚（D7）时返回 L。
///
/// Node 只在上一条的全部消息都是前缀时才这样写（offset 总是上一条的条数）；Anthropic 请求的缓存断点每次后移，上一条
/// 的最后一条消息这一条里不同，那样几乎不生效。App 的读者按 offset 截上一条再接上这一条（`slice(0, offset)`），L 小于
/// 上一条的条数也能展开（D8）。
fn delta_base(previous: &State, current: &State, baseline: usize) -> Option<usize> {
    let common = previous
        .hashes
        .iter()
        .zip(&current.hashes)
        .take_while(|(left, right)| left == right)
        .count();
    (common > 0
        && current.hashes.len() - common <= baseline
        && previous.started_ms <= current.started_ms)
        .then_some(common)
}

/// 一条消息的哈希：序列化的文本逐字节进 SipHash（进程内比较用，不落盘）。
fn hash(message: &Value) -> u64 {
    struct Feed(std::hash::DefaultHasher);
    impl io::Write for Feed {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.write(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut feed = Feed(std::hash::DefaultHasher::new());
    let _ = serde_json::to_writer(&mut feed, message);
    feed.0.finish()
}

#[cfg(test)]
#[path = "model_io_files_tests.rs"]
mod tests;
