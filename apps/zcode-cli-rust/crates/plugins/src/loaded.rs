//! A plugin whose manifest was read (Node `LoadedPlugin`).
use serde_json::Value;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Official,
    Inline,
    Cache,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Official => "official",
            Self::Inline => "inline",
            Self::Cache => "cache",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Loaded {
    /// `name@marketplace`.
    pub id: String,
    pub manifest: Value,
    pub manifest_path: PathBuf,
    pub marketplace: String,
    pub root: PathBuf,
    pub source: Source,
}

impl Loaded {
    pub fn name(&self) -> &str {
        self.manifest["name"].as_str().unwrap_or_default()
    }
}
