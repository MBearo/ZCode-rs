use serde::{Deserialize, Serialize};
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileCheckpoint {
    pub id: String,
    pub path: String,
    pub tool: String,
    pub before: Option<String>,
    pub after: String,
    pub mode: Option<u32>,
    pub row: u64,
    #[serde(default)]
    pub restored: bool,
}
