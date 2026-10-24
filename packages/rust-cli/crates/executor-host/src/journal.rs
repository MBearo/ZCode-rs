//! 文件版账本的日志（docs/specs/rust-cli/remote-exec.md §19.7）：每条记录一行 JSON，只追加；读的时候按顺序重放。
//!
//! 记录（`t`）：`b` 登记（带指纹）、`f` 完成（带编码好的回复帧，base64）、`x` 删掉、`n` 重启前没有终态（中断）、
//! `e` 回复过期（只留键与指纹）。最后一行写到一半（进程在写的时候被杀）时丢掉；中间看不懂的行记日志后跳过。

use std::collections::HashMap;

use base64::Engine;
use bytes::Bytes;
use rust_cli_exec_server::CallKey;
use serde::{Deserialize, Serialize};

/// 一条记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Record {
    pub t: Tag,
    pub i: String,
    pub c: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub f: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r: Option<String>,
    pub at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Tag {
    #[serde(rename = "b")]
    Begin,
    #[serde(rename = "f")]
    Finish,
    #[serde(rename = "x")]
    Forget,
    #[serde(rename = "n")]
    Interrupted,
    #[serde(rename = "e")]
    Expired,
}

/// 一次调用在日志里的状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum State {
    Running,
    Done(Bytes),
    /// 墓碑：结果不明（重启中断）或已经过期；同一个调用 ID 再来时回 `interrupted`。
    Tomb,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub fingerprint: String,
    pub state: State,
    /// 最后一次变化的时刻（Unix 毫秒）：保留规则按它算。
    pub at: u64,
}

impl Entry {
    /// 这一项在日志里大约占多少字节（压缩与大小上限按它估）。
    pub fn weight(&self) -> u64 {
        let reply = match &self.state {
            State::Done(reply) => reply.len() as u64 * 4 / 3,
            State::Running | State::Tomb => 0,
        };
        96 + self.fingerprint.len() as u64 + reply
    }
}

fn encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn decode(text: &str) -> Option<Bytes> {
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .ok()
        .map(Bytes::from)
}

impl Record {
    pub fn begin(key: &CallKey, fingerprint: &str, at: u64) -> Self {
        Self::new(Tag::Begin, key, Some(fingerprint.to_owned()), None, at)
    }

    pub fn finish(key: &CallKey, reply: &[u8], at: u64) -> Self {
        Self::new(Tag::Finish, key, None, Some(encode(reply)), at)
    }

    pub fn mark(tag: Tag, key: &CallKey, at: u64) -> Self {
        Self::new(tag, key, None, None, at)
    }

    fn new(tag: Tag, key: &CallKey, f: Option<String>, r: Option<String>, at: u64) -> Self {
        Self {
            t: tag,
            i: key.instance.clone(),
            c: key.call,
            f,
            r,
            at,
        }
    }

    pub fn key(&self) -> CallKey {
        CallKey {
            instance: self.i.clone(),
            call: self.c,
        }
    }

    /// 一行（带换行）。
    pub fn line(&self) -> String {
        let mut line = serde_json::to_string(self).unwrap_or_default();
        line.push('\n');
        line
    }
}

/// 把一项写回成记录（压缩时用）。
pub(crate) fn records_of(key: &CallKey, entry: &Entry) -> Vec<Record> {
    let mut records = vec![Record::begin(key, &entry.fingerprint, entry.at)];
    match &entry.state {
        State::Running => {}
        State::Done(reply) => records.push(Record::finish(key, reply, entry.at)),
        State::Tomb => records.push(Record::mark(Tag::Expired, key, entry.at)),
    }
    records
}

/// 重放一份日志。返回活着的项与跳过的行数。
pub(crate) fn replay(text: &str) -> (HashMap<CallKey, Entry>, usize) {
    let mut entries: HashMap<CallKey, Entry> = HashMap::new();
    let mut skipped = 0;
    let complete = text.ends_with('\n');
    let lines: Vec<&str> = text.split('\n').filter(|line| !line.is_empty()).collect();
    for (index, line) in lines.iter().enumerate() {
        let Ok(record) = serde_json::from_str::<Record>(line) else {
            // 最后一行没有换行：写到一半，丢掉不算错。
            if index + 1 != lines.len() || complete {
                skipped += 1;
            }
            continue;
        };
        apply(&mut entries, record);
    }
    (entries, skipped)
}

/// 一条记录作用到状态上（重放与运行时同一套规则）。
pub(crate) fn apply(entries: &mut HashMap<CallKey, Entry>, record: Record) {
    let key = record.key();
    match record.t {
        Tag::Begin => {
            entries.insert(
                key,
                Entry {
                    fingerprint: record.f.unwrap_or_default(),
                    state: State::Running,
                    at: record.at,
                },
            );
        }
        Tag::Finish => {
            if let (Some(entry), Some(reply)) =
                (entries.get_mut(&key), record.r.as_deref().and_then(decode))
            {
                entry.state = State::Done(reply);
                entry.at = record.at;
            }
        }
        Tag::Interrupted | Tag::Expired => {
            if let Some(entry) = entries.get_mut(&key) {
                entry.state = State::Tomb;
                entry.at = record.at;
            }
        }
        Tag::Forget => {
            entries.remove(&key);
        }
    }
}
