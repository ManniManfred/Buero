//! Inhaltsadressierte Dateiablage: `files/ab/abcdef…` (SHA-256).

use std::path::{Path, PathBuf};

use axum::body::Bytes;
use futures_util::Stream;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

#[derive(Clone)]
pub struct Storage {
    root: PathBuf,
}

pub struct StagedFile {
    pub tmp_path: PathBuf,
    pub sha256: String,
    pub size: i64,
}

impl Storage {
    pub async fn new(root: &Path) -> std::io::Result<Self> {
        for d in ["files", "thumbs", "tmp"] {
            tokio::fs::create_dir_all(root.join(d)).await?;
        }
        // Reste abgebrochener Uploads entfernen
        if let Ok(mut rd) = tokio::fs::read_dir(root.join("tmp")).await {
            while let Ok(Some(e)) = rd.next_entry().await {
                let _ = tokio::fs::remove_file(e.path()).await;
            }
        }
        Ok(Storage { root: root.to_path_buf() })
    }

    pub fn file_path(&self, sha: &str) -> PathBuf {
        self.root.join("files").join(&sha[..2]).join(sha)
    }

    pub fn thumb_path(&self, doc_id: i64) -> PathBuf {
        self.root.join("thumbs").join(format!("{doc_id}.jpg"))
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// Schreibt einen Datenstrom in eine temporäre Datei und berechnet dabei den Hash.
    pub async fn stage<S, E>(&self, mut stream: S, max: usize) -> anyhow::Result<StagedFile>
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin,
        E: std::fmt::Display,
    {
        let tmp_path = self.tmp_dir().join(format!("up-{:016x}", rand::random::<u64>()));
        let mut file = tokio::io::BufWriter::with_capacity(
            256 * 1024,
            tokio::fs::File::create(&tmp_path).await?,
        );
        let mut hasher = Sha256::new();
        let mut size: usize = 0;
        let res: anyhow::Result<()> = async {
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| anyhow::anyhow!("{e}"))?;
                size += chunk.len();
                if size > max {
                    anyhow::bail!("Datei ist zu groß (max. {} MB)", max / 1024 / 1024);
                }
                hasher.update(&chunk);
                file.write_all(&chunk).await?;
            }
            file.flush().await?;
            Ok(())
        }
        .await;
        if let Err(e) = res {
            drop(file);
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(e);
        }
        Ok(StagedFile {
            tmp_path,
            sha256: hex::encode(hasher.finalize()),
            size: size as i64,
        })
    }

    /// Verschiebt eine gestagte Datei an ihren endgültigen Ort (oder verwirft sie,
    /// falls derselbe Inhalt schon existiert).
    pub async fn commit(&self, staged: &StagedFile) -> std::io::Result<()> {
        let dest = self.file_path(&staged.sha256);
        if tokio::fs::try_exists(&dest).await.unwrap_or(false) {
            tokio::fs::remove_file(&staged.tmp_path).await?;
            return Ok(());
        }
        tokio::fs::create_dir_all(dest.parent().unwrap()).await?;
        tokio::fs::rename(&staged.tmp_path, &dest).await
    }

    pub async fn discard(&self, staged: &StagedFile) {
        let _ = tokio::fs::remove_file(&staged.tmp_path).await;
    }

    pub async fn remove(&self, sha: &str) {
        let _ = tokio::fs::remove_file(self.file_path(sha)).await;
    }
}
