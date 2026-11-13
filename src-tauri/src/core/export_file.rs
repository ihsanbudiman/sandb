//! The one export-file writer. Every export (CSV, INSERT statements, Extended JSON, a table's
//! DDL, the UI's PDF) writes a temp file next to the chosen path, flushes it, and renames it over
//! the path; a failure removes the temp file and never touches an existing file.

use std::path::{Path, PathBuf};

use tokio::io::{AsyncWriteExt, BufWriter};

use crate::core::engine::Engine;
use crate::core::error::AppError;
use crate::core::query_registry::{QueryRegistry, RunGuard};
use crate::core::result::ExportResult;

pub fn check_path(path: &Path) -> Result<(), AppError> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(AppError::invalid("Choose a file to export to"))
    }
}

/// Hidden sibling of `path`, so the final rename stays on one file system.
fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.{}.part", uuid::Uuid::new_v4()))
}

/// A file being written; a database export registers a run so `cancel_query` reaches it.
pub struct ExportFile<'a> {
    path: PathBuf,
    tmp: PathBuf,
    out: BufWriter<tokio::fs::File>,
    run: Option<(String, RunGuard<'a>)>,
    done: bool,
}

impl<'a> ExportFile<'a> {
    /// A local file with no database behind it, e.g. the PDF the UI built.
    pub async fn create(path: &Path) -> Result<Self, AppError> {
        Self::write_to(path, None).await
    }

    /// An export that runs a database read: the run is registered under a fresh id, which the
    /// engine uses as the operation comment and `cancel_query` uses to stop it.
    pub async fn for_query(
        path: &Path,
        queries: &'a QueryRegistry,
        conn_id: &str,
        database: &str,
        engine: Engine,
    ) -> Result<Self, AppError> {
        let query_id = uuid::Uuid::new_v4().to_string();
        let run = queries.register(&query_id, conn_id, database, engine, 1)?;
        Self::write_to(path, Some((query_id, run))).await
    }

    async fn write_to(
        path: &Path,
        run: Option<(String, RunGuard<'a>)>,
    ) -> Result<Self, AppError> {
        check_path(path)?;
        let tmp = temp_path(path);
        let out = BufWriter::new(tokio::fs::File::create(&tmp).await?);
        Ok(Self {
            path: path.to_path_buf(),
            tmp,
            out,
            run,
            done: false,
        })
    }

    pub fn out(&mut self) -> &mut BufWriter<tokio::fs::File> {
        &mut self.out
    }

    /// The id the engine puts in its operation `comment`.
    pub fn query_id(&self) -> Option<&str> {
        self.run.as_ref().map(|(id, _)| id.as_str())
    }

    pub async fn finish(mut self, rows: u64) -> Result<ExportResult, AppError> {
        let cancelled = self
            .run
            .as_ref()
            .is_some_and(|(_, run)| run.cancelled.is_cancelled());
        if cancelled {
            return Err(AppError::cancelled());
        }
        self.out.flush().await?;
        self.out.get_ref().sync_all().await?;
        tokio::fs::rename(&self.tmp, &self.path).await?;
        self.done = true;
        Ok(ExportResult {
            cancelled: false,
            rows,
            path: self.path.display().to_string(),
        })
    }
}

impl Drop for ExportFile<'_> {
    fn drop(&mut self) {
        if !self.done {
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_finished_file_replaces_only_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.csv");
        std::fs::write(&path, b"old").unwrap();
        let mut f = ExportFile::create(&path).await.unwrap();
        f.out().write_all(b"a,b\r\n").await.unwrap();
        let r = f.finish(1).await.unwrap();
        assert!(!r.cancelled);
        assert_eq!(r.rows, 1);
        assert_eq!(std::fs::read(&path).unwrap(), b"a,b\r\n");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn a_dropped_file_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.csv");
        {
            let f = ExportFile::create(&path).await.unwrap();
            drop(f);
        }
        assert!(!path.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_relative_path_is_refused() {
        let e = check_path(Path::new("relative.csv")).unwrap_err();
        assert_eq!(e.code, crate::core::error::ErrorCode::InvalidInput);
    }
}
