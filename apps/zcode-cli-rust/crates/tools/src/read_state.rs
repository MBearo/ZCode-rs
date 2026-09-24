//! Node `readFileState` views of one session: text Reads by `(path, offset,
//! limit)` and edits, re-attached after compaction (spec rust-m7-compact §6.2).
use crate::domain::compact_ptl::ReadView;
use std::path::PathBuf;

/// Views kept per session; older ones go first (only re-attachment candidates).
const MAX_VIEWS: usize = 256;

struct View {
    key: (PathBuf, u64, Option<u64>),
    /// `None`: the latest state came from Edit or Write (not a candidate).
    read: Option<ReadView>,
}

#[derive(Default)]
pub struct Views {
    list: Vec<View>,
}

impl Views {
    /// A Read (`read`) or an edit of `path`; the newest view of a key replaces the older one.
    pub(super) fn record(
        &mut self,
        path: PathBuf,
        (offset, limit): (u64, Option<u64>),
        read: Option<ReadView>,
    ) {
        let key = (path, offset, limit);
        self.list.retain(|view| view.key != key);
        if self.list.len() >= MAX_VIEWS {
            self.list.remove(0);
        }
        self.list.push(View { key, read });
    }

    /// The Read views, newest first; the state is emptied (Node `readFileState.clear()`).
    pub(super) fn take(&mut self) -> Vec<ReadView> {
        std::mem::take(&mut self.list)
            .into_iter()
            .rev()
            .filter_map(|view| view.read)
            .collect()
    }
}
