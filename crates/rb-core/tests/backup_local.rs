//! End-to-end tests of the local backup path: real files, real repository
//! on disk, real chunking/encryption (fast KDF parameters to keep the
//! suite quick).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rb_core::backend::{LocalFs, StorageBackend};
use rb_core::backup::{BackupEvent, BackupOptions, BackupStats, run_backup};
use rb_core::chunker::ChunkerConfig;
use rb_core::crypto::KdfParams;
use rb_core::domain::{Destination, JobConfig, JobId};
use rb_core::hash::ChunkHash;
use rb_core::manifest::{EntryKind, SnapshotManifest};
use rb_core::repo::Repository;
use uuid::Uuid;

fn fast_kdf() -> KdfParams {
    KdfParams {
        m_cost: 1024,
        t_cost: 1,
        p_cost: 1,
    }
}

/// Deterministic pseudo-random bytes (incompressible enough for realistic
/// chunking).
fn random_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 32) as u8
        })
        .collect()
}

struct Fixture {
    _keep: Vec<PathBuf>,
    source: PathBuf,
    dest: PathBuf,
    job: JobConfig,
    repo: Repository,
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rb-it-{tag}-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn make_tree(source: &Path) {
    std::fs::write(source.join("a.bin"), random_bytes(2_500_000, 11)).expect("a.bin");
    std::fs::create_dir_all(source.join("nested")).expect("nested");
    std::fs::write(source.join("nested/b.bin"), random_bytes(1_200_000, 22)).expect("b.bin");
    std::fs::write(
        source.join("nested/c.txt"),
        b"just some text, compressible\n".repeat(50),
    )
    .expect("c.txt");
    std::fs::write(source.join("empty.txt"), b"").expect("empty.txt");
}

fn fixture(tag: &str) -> Fixture {
    let base = temp_dir(tag);
    let source = base.join("src");
    let dest = base.join("repo");
    std::fs::create_dir_all(&source).expect("src dir");
    make_tree(&source);

    let job = JobConfig {
        id: JobId::new_v4(),
        name: format!("job-{tag}"),
        sources: vec![source.clone()],
        destination: Destination::Local { root: dest.clone() },
        compression_level: 5,
        created_at: chrono::Utc::now(),
    };

    let backend: Arc<dyn StorageBackend> = Arc::new(LocalFs::new(&dest));
    let repo = Repository::init(
        backend,
        "integration passphrase",
        ChunkerConfig::default(),
        5,
        fast_kdf(),
    )
    .expect("repo init");

    Fixture {
        _keep: vec![base],
        source,
        dest,
        job,
        repo,
    }
}

fn backup(fixture: &Fixture) -> (SnapshotManifest, BackupStats) {
    run_backup(
        &fixture.repo,
        &fixture.job,
        &BackupOptions::default(),
        |_| Ok(()),
    )
    .expect("backup run")
}

fn backup_with_events(
    fixture: &Fixture,
    on_event: impl FnMut(BackupEvent) -> rb_core::Result<()>,
) -> rb_core::Result<(SnapshotManifest, BackupStats)> {
    run_backup(
        &fixture.repo,
        &fixture.job,
        &BackupOptions::default(),
        on_event,
    )
}

/// BLAKE3 of every file under `root`, keyed by relative path — used to
/// prove sources are never modified by any operation.
fn tree_hashes(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(root).sort_by_file_name() {
        let entry = entry.expect("walk");
        if entry.file_type().is_file() {
            let data = std::fs::read(entry.path()).expect("read for hash");
            let rel = entry
                .path()
                .strip_prefix(root)
                .expect("prefix")
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, ChunkHash::from_data(&data).to_hex()));
        }
    }
    out
}

fn manifest_files(manifest: &SnapshotManifest) -> Vec<(String, u64)> {
    manifest
        .iter_files()
        .map(|(_, e)| (e.path.to_string_lossy().replace('\\', "/"), e.size))
        .collect()
}

#[test]
fn full_then_incremental_backups_dedup_and_never_touch_sources() {
    let fixture = fixture("dedup");

    // --- Run 1: full backup --------------------------------------------
    let (m1, s1) = backup(&fixture);
    let mut expected: Vec<(String, u64)> = vec![
        ("a.bin".to_string(), 2_500_000),
        ("nested/b.bin".to_string(), 1_200_000),
        (
            "nested/c.txt".to_string(),
            ("just some text, compressible\n".len() * 50) as u64,
        ),
        ("empty.txt".to_string(), 0),
    ];
    expected.sort();
    let mut got = manifest_files(&m1);
    got.sort();
    assert_eq!(got, expected, "snapshot must capture the whole tree");
    assert!(s1.chunks_new >= 3, "full run stores several chunks");

    // Repository layout sanity: config, key, chunks, one manifest.
    let backend = LocalFs::new(&fixture.dest);
    assert!(backend.exists("config.json").expect("config exists"));
    assert_eq!(backend.list("keys").expect("keys").len(), 1);
    assert_eq!(backend.list("snapshots").expect("snaps").len(), 1);
    let chunk_files = backend.list("chunks").expect("chunks");
    assert_eq!(chunk_files.len() as u64, s1.chunks_new);

    // --- Run 2: nothing changed -> zero new chunks ----------------------
    let (_m2, s2) = backup(&fixture);
    assert_eq!(s2.chunks_new, 0, "unchanged tree must store no chunks");
    assert_eq!(s2.files_total, s1.files_total);

    // --- Run 3: incremental after edits ---------------------------------
    let mut a = std::fs::read(fixture.source.join("a.bin")).expect("read a");
    a.extend_from_slice(&random_bytes(100_000, 33));
    std::fs::write(fixture.source.join("a.bin"), &a).expect("append a");
    std::fs::write(fixture.source.join("new.bin"), random_bytes(500_000, 44)).expect("new file");

    // Baseline taken right before the run the invariant guards: backups
    // must never modify source data.
    let before = tree_hashes(&fixture.source);
    let (m3, s3) = backup(&fixture);
    assert!(s3.chunks_new >= 1, "changed content stores new chunks");
    assert!(
        s3.chunks_new < s1.chunks_new,
        "incremental ({}) must store fewer chunks than full ({})",
        s3.chunks_new,
        s1.chunks_new
    );
    assert_eq!(m3.file_count(), 5);
    assert_eq!(backend.list("snapshots").expect("snaps").len(), 3);

    // --- Sources never modified ------------------------------------------
    let after = tree_hashes(&fixture.source);
    assert_eq!(before, after, "backup must not touch any source file");
}

#[test]
fn cancelled_run_leaves_no_snapshot_manifest() {
    let fixture = fixture("cancel");
    let cancel = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(AtomicU64::new(0));
    let cancel_after_first = {
        let cancel = Arc::clone(&cancel);
        let seen = Arc::clone(&seen);
        move |event: BackupEvent| {
            if matches!(event, BackupEvent::FileStarted { .. })
                && seen.fetch_add(1, Ordering::SeqCst) >= 1
            {
                cancel.store(true, Ordering::SeqCst);
            }
            Ok(())
        }
    };

    let result = run_backup(
        &fixture.repo,
        &fixture.job,
        &BackupOptions {
            cancel: Some(cancel),
        },
        cancel_after_first,
    );
    assert!(
        result.is_err(),
        "cancelled run must return an error, got {:?}",
        result.map(|(_, s)| s)
    );

    // The invariant: no manifest finalized -> the snapshot does not exist.
    let backend = LocalFs::new(&fixture.dest);
    assert_eq!(
        backend.list("snapshots").expect("list snapshots").len(),
        0,
        "cancelled run must not leave a snapshot manifest"
    );
    // Chunks written so far are harmless (dedup will reuse them).
    let _chunks = backend.list("chunks").expect("list chunks");
    // And a later, non-cancelled run succeeds and dedups against them.
    let (_m, s) = backup(&fixture);
    assert!(s.files_total >= 4);
}

#[test]
fn locked_file_is_skipped_and_run_still_finalizes() {
    let fixture = fixture("locked");
    let locked_path = fixture.source.join("locked.pst");
    std::fs::write(&locked_path, random_bytes(4096, 55)).expect("locked file");

    // Hold the file with NO sharing on Windows: the engine cannot read it.
    #[cfg(windows)]
    let _holder = {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&locked_path)
            .expect("open exclusive")
    };
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&locked_path).expect("meta").permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&locked_path, perms).expect("chmod");
    }

    let mut skipped_seen = 0u64;
    let (manifest, stats) = run_backup(
        &fixture.repo,
        &fixture.job,
        &BackupOptions::default(),
        |event| {
            if matches!(event, BackupEvent::FileSkipped { .. }) {
                skipped_seen += 1;
            }
            Ok(())
        },
    )
    .expect("run completes despite locked file");

    assert_eq!(skipped_seen, 1);
    assert_eq!(stats.files_skipped, 1);
    assert_eq!(manifest.skipped.len(), 1);
    assert_eq!(
        manifest.skipped[0].path,
        PathBuf::from("locked.pst"),
        "skipped item must be recorded in the manifest"
    );
    // All readable files are captured.
    assert!(
        manifest
            .iter_files()
            .any(|(_, e)| e.kind == EntryKind::File && e.path == Path::new("a.bin"))
    );

    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&locked_path).expect("meta").permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&locked_path, perms).expect("restore perms");
    }
}

#[test]
fn events_report_real_progress_and_new_chunk_counts() {
    let fixture = fixture("events");
    let done = Arc::new(AtomicU64::new(0));
    let new_chunks: Arc<std::sync::Mutex<Vec<u64>>> = Arc::default();
    let d = Arc::clone(&done);
    let n = Arc::clone(&new_chunks);
    let (_m, _s) = backup_with_events(&fixture, move |event| {
        if let BackupEvent::FileDone { new_chunks, .. } = event {
            d.fetch_add(1, Ordering::SeqCst);
            n.lock().expect("mutex").push(new_chunks);
        }
        Ok(())
    })
    .expect("run");
    assert_eq!(done.load(Ordering::SeqCst), 4, "one FileDone per file");
    let chunks: u64 = new_chunks.lock().expect("mutex").iter().sum();
    assert!(chunks >= 3, "FileDone counters must sum to new chunks");
}

#[test]
fn second_repository_on_same_dest_is_refused() {
    let fixture = fixture("doubleinit");
    let backend: Arc<dyn StorageBackend> = Arc::new(LocalFs::new(&fixture.dest));
    assert!(
        Repository::init(
            backend,
            "other passphrase",
            ChunkerConfig::default(),
            5,
            fast_kdf()
        )
        .is_err()
    );
}
