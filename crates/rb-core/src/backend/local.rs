//! Local-filesystem backend: USB disks, internal drives, mounted shares.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use uuid::Uuid;
use walkdir::WalkDir;

use super::{ChunkWriter, StorageBackend};

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

    fn read_range(&self, rel: &str, offset: u64, len: u64) -> crate::Result<Vec<u8>> {
        use std::io::{Read as _, Seek as _, SeekFrom};
        let mut file = std::fs::File::open(self.resolve(rel))?;
        file.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; usize::try_from(len).map_err(std::io::Error::other)?];
        file.read_exact(&mut buf)?;
        Ok(buf)
    }

    fn read_ranges(&self, rel: &str, ranges: &[(u64, u64)]) -> crate::Result<Vec<Vec<u8>>> {
        use std::io::{Read as _, Seek as _, SeekFrom};
        let mut file = std::fs::File::open(self.resolve(rel))?;
        let mut out = Vec::with_capacity(ranges.len());
        for &(offset, len) in ranges {
            file.seek(SeekFrom::Start(offset))?;
            let mut buf = vec![0u8; usize::try_from(len).map_err(std::io::Error::other)?];
            file.read_exact(&mut buf)?;
            out.push(buf);
        }
        Ok(out)
    }

    fn write_atomic(&self, rel: &str, data: &[u8]) -> crate::Result<()> {
        let target = self.resolve(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = target.with_extension(format!("tmp-{}", Uuid::new_v4().simple()));
        // Durable write: the bytes reach the device (sync_all) BEFORE the
        // rename publishes the name. Without it, unplugging a USB disk can
        // leave a renamed but truncated chunk that dedup would trust forever.
        let written = (|| -> std::io::Result<()> {
            use std::io::Write as _;
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(data)?;
            file.sync_all()
        })();
        // `std::fs::rename` replaces an existing target atomically on both
        // Windows (MoveFileExW + REPLACE_EXISTING) and POSIX: there is never a
        // window where the target name is missing.
        let renamed = written.and_then(|()| std::fs::rename(&tmp, &target));
        if let Err(err) = renamed {
            let _ = std::fs::remove_file(&tmp);
            return Err(err.into());
        }
        Ok(())
    }

    fn exists(&self, rel: &str) -> crate::Result<bool> {
        // `try_exists`, not `exists`: an I/O error (permission denied, device
        // gone mid-call) must surface instead of reading as "absent".
        Ok(self.resolve(rel).try_exists()?)
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

    fn remove(&self, rel: &str) -> crate::Result<()> {
        let target = self.resolve(rel);
        if target.exists() {
            std::fs::remove_file(&target)?;
        }
        Ok(())
    }

    fn open_writer(&self) -> crate::Result<Box<dyn ChunkWriter>> {
        Ok(Box::new(LocalChunkWriter {
            root: self.root.clone(),
            dirs: HashSet::new(),
        }))
    }
}

/// Parallel chunk writer for [`LocalFs`]: same durability as
/// [`StorageBackend::write_atomic`] (temp, sync, rename) minus the redundant
/// directory probing — parents are created once and remembered.
struct LocalChunkWriter {
    root: PathBuf,
    dirs: HashSet<PathBuf>,
}

impl ChunkWriter for LocalChunkWriter {
    fn write_new(&mut self, rel: &str, data: &[u8]) -> crate::Result<()> {
        let target = self
            .root
            .join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        if let Some(parent) = target.parent()
            && !self.dirs.contains(parent)
        {
            std::fs::create_dir_all(parent)?;
            self.dirs.insert(parent.to_path_buf());
        }
        let tmp = target.with_extension(format!("tmp-{}", Uuid::new_v4().simple()));
        let written = (|| -> std::io::Result<()> {
            use std::io::Write as _;
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(data)?;
            file.sync_all()
        })();
        // Local rename replaces an existing target atomically; the pipeline
        // guarantees the target does not exist, so this is a plain publish.
        if let Err(err) = written.and_then(|()| std::fs::rename(&tmp, &target)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(err.into());
        }
        Ok(())
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
    fn write_atomic_replaces_target_without_gap() {
        let dir = std::env::temp_dir().join(format!("rb-localbe-{}", Uuid::new_v4()));
        let be = LocalFs::new(&dir);
        be.write_atomic("snapshots/s.snap", b"old").expect("write");
        // A failing write (parent path is a FILE) must neither touch the
        // existing target nor leave a temp file behind.
        be.write_atomic("snapshots/s.snap/child", b"x")
            .expect_err("cannot create a child of a file");
        assert_eq!(be.read("snapshots/s.snap").expect("read"), b"old");
        // A successful overwrite replaces the content in one rename.
        be.write_atomic("snapshots/s.snap", b"new")
            .expect("rewrite");
        assert_eq!(be.read("snapshots/s.snap").expect("read"), b"new");
        assert_eq!(
            be.list("snapshots").expect("list"),
            vec!["snapshots/s.snap".to_string()]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_range_returns_only_requested_bytes_and_errors_on_short_file() {
        let dir = std::env::temp_dir().join(format!("rb-localbe-{}", Uuid::new_v4()));
        let be = LocalFs::new(&dir);
        be.write_atomic("packs/ab/x.pack", b"0123456789")
            .expect("write");
        assert_eq!(
            be.read_range("packs/ab/x.pack", 3, 4).expect("range"),
            b"3456"
        );
        assert_eq!(be.read_range("packs/ab/x.pack", 10, 0).expect("empty"), b"");
        assert!(
            be.read_range("packs/ab/x.pack", 8, 4).is_err(),
            "short file"
        );
        assert!(be.read_range("packs/ab/missing.pack", 0, 1).is_err());
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
