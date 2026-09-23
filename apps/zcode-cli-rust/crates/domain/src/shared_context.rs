use super::session::StoredAttachment;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const MAX_BYTES: usize = 20 * 1024 * 1024;
#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Pending,
    Reserved,
    Attached,
    Discarded,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstalledArtifact {
    pub artifact_id: String,
    pub workspace_relative_path: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Provenance {
    pub share_id: String,
    pub context_id: Option<String>,
    pub share_url: Option<String>,
    #[serde(default)]
    pub status: Status,
    pub projection_sha256: String,
    pub artifact_set_sha256: String,
    pub formatter_version: u8,
    pub markdown_sha256: String,
    pub installed_artifacts: Vec<InstalledArtifact>,
}
pub fn nonempty(value: &mut String) -> Result<()> {
    *value = value.trim().to_owned();
    ensure!(!value.is_empty(), "Empty shared context identity");
    Ok(())
}
impl Provenance {
    pub fn validate(&mut self, session: &str, markdown: &str) -> Result<()> {
        nonempty(&mut self.share_id)?;
        let id = self
            .context_id
            .get_or_insert_with(|| format!("legacy-shared-context-{session}"));
        nonempty(id)?;
        ensure!(
            self.formatter_version == 1,
            "Unsupported shared context formatter"
        );
        for hash in [
            &self.projection_sha256,
            &self.artifact_set_sha256,
            &self.markdown_sha256,
        ] {
            ensure!(
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
                "Invalid shared context digest"
            );
        }
        if let Some(value) = &self.share_url {
            let url = url::Url::parse(value)?;
            let code = url.path().strip_prefix("/cn/share/");
            ensure!(
                matches!(url.scheme(), "http" | "https")
                    && url.host_str().is_some()
                    && url.query().is_none()
                    && url.fragment().is_none()
                    && code.is_some_and(|c| !c.is_empty() && !c.contains('/')),
                "Invalid canonical share URL"
            );
        }
        for artifact in &mut self.installed_artifacts {
            nonempty(&mut artifact.artifact_id)?;
            nonempty(&mut artifact.workspace_relative_path)?;
        }
        self.check_content(markdown)
    }
    pub fn check_content(&self, markdown: &str) -> Result<()> {
        ensure!(
            !markdown.trim().is_empty() && markdown.len() <= MAX_BYTES,
            "Invalid shared context size"
        );
        ensure!(
            format!("{:x}", Sha256::digest(markdown.as_bytes())) == self.markdown_sha256,
            "Shared context digest mismatch"
        );
        Ok(())
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedContext {
    pub provenance: Provenance,
    pub content: StoredAttachment,
    pub source_id: Option<String>,
    pub attached_message_id: Option<String>,
}
impl SharedContext {
    pub fn check(&self, id: &str, source: Option<&str>) -> Result<()> {
        ensure!(
            self.provenance.context_id.as_deref() == Some(id)
                && (self.provenance.status == Status::Pending
                    || (self.provenance.status == Status::Reserved
                        && source.is_some()
                        && self.source_id.as_deref() == source)),
            "fault.command.sharedContextNotAttachable"
        );
        Ok(())
    }
    pub fn release(&mut self, source: Option<&str>) {
        if self.provenance.status == Status::Reserved
            && source.is_none_or(|s| self.source_id.as_deref() == Some(s))
        {
            self.provenance.status = Status::Pending;
            self.source_id = None;
        }
    }
    pub fn projection(&self, title: &str) -> Value {
        match (&self.provenance.context_id, &self.provenance.share_url) {
            (Some(id), Some(url)) => {
                json!({"contextId":id,"title":title,"shareUrl":url,"status":self.provenance.status})
            }
            _ => json!({"title":title}),
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    kind: String,
    context_id: String,
}
pub fn reference(p: &Value) -> Result<Option<String>> {
    let Some(value) = p.get("context_refs") else {
        return Ok(None);
    };
    let refs = value.as_array().context("Invalid context_refs")?;
    ensure!(
        refs.len() <= 1,
        "Only one shared context reference is allowed"
    );
    refs.first()
        .map(|value| {
            let mut r: Reference = serde_json::from_value(value.clone())?;
            ensure!(
                r.kind == "shared_context_import",
                "Invalid shared context kind"
            );
            nonempty(&mut r.context_id)?;
            Ok(r.context_id)
        })
        .transpose()
}
