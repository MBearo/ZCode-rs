use super::storage::Store;
use crate::domain::session::StoredAttachment;
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

impl Store {
    pub(super) async fn snapshot_file(&self, path: &str, mime: &str) -> Result<StoredAttachment> {
        let path = if path.starts_with("file://") {
            reqwest::Url::parse(path)?
                .to_file_path()
                .map_err(|_| anyhow::anyhow!("Invalid attachment file URL"))?
        } else {
            path.into()
        };
        let mut file = tokio::fs::File::open(&path).await?;
        let before = file.metadata().await?;
        ensure!(
            before.is_file() && before.len() <= 20 * 1024 * 1024,
            "Attachment must be a regular file within 20 MiB"
        );
        let mut chunks = Vec::new();
        let mut total = 0;
        loop {
            let mut chunk = vec![0; 512 * 1024];
            let count = file.read(&mut chunk).await?;
            if count == 0 {
                break;
            }
            total += count as u64;
            ensure!(
                total <= 20 * 1024 * 1024,
                "Attachment grew beyond size limit"
            );
            chunk.truncate(count);
            chunks.push(chunk);
        }
        let after = file.metadata().await?;
        // 同一次 admission 不能把变更中的文件伪装成稳定快照；排队提升只读取已提交副本。
        ensure!(
            total == before.len()
                && after.len() == before.len()
                && after.modified()? == before.modified()?,
            "Attachment changed during snapshot"
        );
        let mut asset = self.save_attachment(&chunks, mime).await?;
        asset.source_path = Some(path.to_string_lossy().into_owned());
        Ok(asset)
    }
    pub(super) async fn save_attachment(
        &self,
        chunks: &[Vec<u8>],
        mime: &str,
    ) -> Result<StoredAttachment> {
        let total: usize = chunks.iter().map(Vec::len).sum();
        ensure!(total <= 20 * 1024 * 1024, "Attachment exceeds size limit");
        let mut hash = Sha256::new();
        for chunk in chunks {
            hash.update(chunk);
        }
        let path = self.attachment_root.join(format!("{:x}", hash.finalize()));
        tokio::fs::create_dir_all(&self.attachment_root).await?;
        let temp = self.attachment_root.join(super::id());
        let write = async {
            let mut options = tokio::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = options.open(&temp).await?;
            for chunk in chunks {
                file.write_all(chunk).await?;
            }
            file.sync_all().await?;
            drop(file);
            // 内容寻址且不可变，已有同一内容可复用；不会覆盖正在被历史引用的不同字节。
            if tokio::fs::try_exists(&path).await? {
                tokio::fs::remove_file(&temp).await?;
            } else {
                tokio::fs::rename(&temp, &path).await?;
            }
            #[cfg(unix)]
            tokio::fs::File::open(&self.attachment_root)
                .await?
                .sync_all()
                .await?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if write.is_err() {
            let _ = tokio::fs::remove_file(&temp).await;
        }
        write?;
        Ok(StoredAttachment {
            path: path.to_string_lossy().into_owned(),
            source_path: None,
            media_type: mime.into(),
            total_bytes: total as u64,
        })
    }
}
