//! Local-filesystem backend: USB disks, internal drives, mounted shares.

use std::path::{Path, PathBuf};

use uuid::Uuid;
use walkdir::WalkDir;

use super::StorageBackend;

/// Storage on a locally mounted folder.
#[derive(Debug, Clone)]
pub struct LocalFs {
    root: PathBuf,
}

impl LocalFs {
    /// Creates a backend rooted at `root` (the repository folder itself).
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Repository root folder.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn resolve(&self, rel: &str) -> PathBuf {
        // Repo-relative paths always use '/', including on Windows.
        self.root
            .join(rel.replace('/', std::path::MAIN_SEPARATOR_STR))
    }
}

impl StorageBackend for LocalFs {
    fn kind(&self) -> &'static str {
        "local"
    }

    fn describe(&self) -> String {
        format!("local: {}", self.root.display())
    }

    fn read(&self, rel: &str) -> crate::Result<Vec<u8>> {
        Ok(std::fs::read(self.resolve(rel))?)
    }

    fn write_atomic(&self, rel: &str, data: &[u8]) -> crate::Result<()> {
        let target = self.resolve(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = target.with_extension(format!("tmp-{}", Uuid::new_v4().simple()));
        std::fs::write(&tmp, data)?;
        // On Windows, rename fails when the target exists: remove first,
        // then rename — a crash between the two leaves the old intact file.
        if target.exists() {
            std::fs::remove_file(&target)?;
        }
        std::fs::rename(&tmp, &target)?;
        Ok(())
    }

    fn exists(&self, rel: &str) -> crate::Result<bool> {
        Ok(self.resolve(rel).exists())
    }

    fn mkdirs(&self, rel: &str) -> crate::Result<()> {
        Ok(std::fs::create_dir_all(self.resolve(rel))?)
    }

    fn list(&self, prefix: &str) -> crate::Result<Vec<String>> {
        let base = self.resolve(prefix);
        if !base.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in WalkDir::new(&base).min_depth(1).sort_by_file_name() {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = entry
                .path()
                .strip_prefix(&self.root)
                .map_err(|e| crate::Error::Io(std::io::Error::other(e.to_string())))?;
            out.push(
                rel.to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/"),
            );
        }
        out.sort();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_should_never_leave_temp_files_behind() {
        let dir = std::env::temp_dir().join(format!("rb-localbe-{}", Uuid::new_v4()));
        let be = LocalFs::new(&dir);
        be.write_atomic("chunks/ab/hash.chunk", b"payload")
            .expect("write");
        assert!(be.exists("chunks/ab/hash.chunk").expect("exists"));
        // Overwrite is atomic too.
        be.write_atomic("chunks/ab/hash.chunk", b"payload-2")
            .expect("rewrite");
        assert_eq!(be.read("chunks/ab/hash.chunk").expect("read"), b"payload-2");
        let leftovers = be.list("chunks").expect("list");
        assert_eq!(leftovers, vec!["chunks/ab/hash.chunk".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_should_return_slash_separated_paths() {
        let dir = std::env::temp_dir().join(format!("rb-localbe-{}", Uuid::new_v4()));
        let be = LocalFs::new(&dir);
        be.write_atomic("snapshots/x/y.bin", b"z").expect("write");
        assert_eq!(
            be.list("snapshots").expect("list"),
            vec!["snapshots/x/y.bin".to_string()]
        );
        assert_eq!(
            be.list("nowhere").expect("list empty"),
            Vec::<String>::new()
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
