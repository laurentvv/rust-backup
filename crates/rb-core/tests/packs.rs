//! Pack-file repository format (v2): layout, mixed v1/v2 repositories,
//! crash consistency, prune/repack and verify. Real files, real local
//! repository, fast KDF.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rb_core::backend::{ChunkWriter, LocalFs, StorageBackend};
use rb_core::backup::{BackupOptions, run_backup};
use rb_core::chunker::ChunkerConfig;
use rb_core::crypto::KdfParams;
use rb_core::domain::{Destination, JobConfig, JobId};
use rb_core::hash::ChunkHash;
use rb_core::manifest::{ChunkRef, EntryKind, FileEntry, SnapshotManifest, SourceSnapshot};
use rb_core::pack::{BlobEntry, IndexPack, pack_id_of};
use rb_core::repo::Repository;
use rb_core::restore::{RestoreDestination, RestoreOptions, RestoreSelection, restore};
use rb_core::verify::{VerifyMode, verify};
use uuid::Uuid;

const PASS: &str = "pack passphrase";

fn fast_kdf() -> KdfParams {
    KdfParams {
        m_cost: 1024,
        t_cost: 1,
        p_cost: 1,
    }
}

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

struct Env {
    base: PathBuf,
    dest: PathBuf,
    repo: Repository,
}

fn env(tag: &str) -> Env {
    let base = std::env::temp_dir().join(format!("rb-pack-{tag}-{}", Uuid::new_v4()));
    let dest = base.join("repo");
    std::fs::create_dir_all(&base).expect("base");
    let repo = Repository::init(
        Arc::new(LocalFs::new(&dest)),
        PASS,
        ChunkerConfig::default(),
        5,
        fast_kdf(),
    )
    .expect("init");
    Env { base, dest, repo }
}

fn job(env: &Env, name: &str, source: &Path) -> JobConfig {
    JobConfig {
        id: JobId::new_v4(),
        name: name.into(),
        sources: vec![source.to_path_buf()],
        destination: Destination::Local {
            root: env.dest.clone(),
        },
        compression_level: 5,
        excludes: vec![],
        max_incrementals: 10,
        created_at: chrono::Utc::now(),
    }
}

fn source_dir(env: &Env, name: &str) -> PathBuf {
    let dir = env.base.join(name);
    std::fs::create_dir_all(&dir).expect("source dir");
    dir
}

fn run(repo: &Repository, job: &JobConfig) -> (SnapshotManifest, rb_core::backup::BackupStats) {
    run_backup(repo, job, &BackupOptions::default(), |_| Ok(())).expect("backup")
}

fn restore_to(
    repo: &Repository,
    manifest: &SnapshotManifest,
    source: &Path,
    rel: &str,
    target: &Path,
) -> rb_core::Result<rb_core::restore::RestoreStats> {
    restore(
        repo,
        &manifest.id,
        &[RestoreSelection {
            source_root: source.to_path_buf(),
            rel_path: PathBuf::from(rel),
        }],
        &RestoreDestination::Target(target.to_path_buf()),
        &RestoreOptions::default(),
        |_| Ok(()),
    )
}

fn files_in(dest: &Path, dir: &str, suffix: &str) -> Vec<String> {
    LocalFs::new(dest)
        .list(dir)
        .expect("list")
        .into_iter()
        .filter(|rel| rel.ends_with(suffix))
        .collect()
}

fn cleanup(env: &Env) {
    std::fs::remove_dir_all(&env.base).ok();
}

/// P1 + P2 (local): 1 200 small files land in at most 2 packs and zero
/// loose chunks; a single file and a directory restore byte-identically.
#[test]
fn p1_p2_small_files_share_packs_and_restore_byte_identical() {
    let env = env("small");
    let source = source_dir(&env, "src");
    std::fs::create_dir_all(source.join("d")).expect("d");
    for i in 0u64..1200 {
        let name = if i % 2 == 0 {
            format!("f{i:04}.bin")
        } else {
            format!("d/f{i:04}.bin")
        };
        std::fs::write(
            source.join(name),
            random_bytes(1500 + (i as usize % 7), i + 1),
        )
        .expect("small file");
    }
    let job = job(&env, "small", &source);
    let (manifest, stats) = run(&env.repo, &job);
    assert_eq!(stats.chunks_new, 1200);

    let packs = files_in(&env.dest, "packs", ".pack");
    assert!(
        (1..=2).contains(&packs.len()),
        "expected at most 2 packs, got {}",
        packs.len()
    );
    assert!(
        LocalFs::new(&env.dest)
            .list("chunks")
            .expect("chunks")
            .is_empty(),
        "no loose chunks"
    );
    assert_eq!(files_in(&env.dest, "index", ".idx").len(), 1);

    // Single file.
    let t1 = env.base.join("t1");
    restore_to(&env.repo, &manifest, &source, "f0004.bin", &t1).expect("restore file");
    assert_eq!(
        std::fs::read(t1.join("src/f0004.bin")).expect("restored"),
        std::fs::read(source.join("f0004.bin")).expect("original")
    );
    // Directory.
    let t2 = env.base.join("t2");
    let s = restore_to(&env.repo, &manifest, &source, "d", &t2).expect("restore dir");
    assert_eq!(s.files_restored, 600);
    for i in (1u64..1200).step_by(2) {
        let name = format!("d/f{i:04}.bin");
        assert_eq!(
            std::fs::read(t2.join("src").join(&name)).expect("restored"),
            std::fs::read(source.join(&name)).expect("original"),
            "{name}"
        );
    }
    // Reopening the repository (fresh chunk map) reads the same data.
    let reopened = Repository::open(Arc::new(LocalFs::new(&env.dest)), PASS).expect("open");
    let report = verify(&reopened, VerifyMode::Full, None, |_, _| Ok(())).expect("verify");
    assert!(report.ok(), "{:?}", report.issues);
    assert_eq!(report.unindexed_packs, 0);
    cleanup(&env);
}

/// P4: a v1 repository (loose chunks, config version 1) keeps working: new
/// writes go to packs, config is upgraded, old and new snapshots restore.
#[test]
fn p4_mixed_repository_restores_old_and_new_snapshots() {
    let env = env("mixed");
    let backend = LocalFs::new(&env.dest);

    // Fabricate a v1 snapshot: one loose chunk + a manifest referencing it.
    let old_root = source_dir(&env, "oldsrc");
    let old_data = random_bytes(100_000, 5);
    let old_hash = ChunkHash::from_data(&old_data);
    assert!(env.repo.store_chunk(&old_hash, &old_data).expect("loose"));
    let mut old_manifest = SnapshotManifest::new(Uuid::new_v4(), "old-job");
    old_manifest.sources.push(SourceSnapshot {
        root: old_root.clone(),
        entries: vec![FileEntry {
            path: PathBuf::from("old.bin"),
            size: old_data.len() as u64,
            mtime_ns: 0,
            kind: EntryKind::File,
            chunks: vec![ChunkRef {
                hash: old_hash,
                plain_len: old_data.len() as u64,
            }],
        }],
    });
    env.repo.write_manifest(&old_manifest).expect("manifest");

    // Downgrade the config to version 1, as an old binary would have left it.
    let mut config = env.repo.config().clone();
    config.version = 1;
    backend
        .write_atomic(
            "config.json",
            serde_json::to_string_pretty(&config)
                .expect("json")
                .as_bytes(),
        )
        .expect("config v1");
    let repo = Repository::open(Arc::new(LocalFs::new(&env.dest)), PASS).expect("open v1");

    // New backup, including a file identical to the loose chunk (dedup
    // must see loose chunks).
    let source = source_dir(&env, "newsrc");
    std::fs::write(source.join("new.bin"), random_bytes(600_000, 6)).expect("new");
    std::fs::write(source.join("same_as_old.bin"), &old_data).expect("same");
    let job = job(&env, "new-job", &source);
    let (new_manifest, stats) = run(&repo, &job);
    let new_blobs = repo.packed_blob_count().expect("blobs");
    assert_eq!(
        new_blobs, stats.chunks_new,
        "loose chunk deduped, not repacked"
    );

    let on_disk: rb_core::repo::RepoConfig =
        serde_json::from_slice(&backend.read("config.json").expect("config")).expect("json");
    assert_eq!(on_disk.version, 2, "first pack write upgrades the config");
    assert_eq!(backend.list("chunks").expect("chunks").len(), 1);
    assert!(!files_in(&env.dest, "packs", ".pack").is_empty());

    let t_old = env.base.join("t_old");
    restore_to(&repo, &old_manifest, &old_root, "old.bin", &t_old).expect("restore old");
    assert_eq!(
        std::fs::read(t_old.join("oldsrc/old.bin")).expect("old"),
        old_data
    );
    let t_new = env.base.join("t_new");
    for name in ["new.bin", "same_as_old.bin"] {
        restore_to(&repo, &new_manifest, &source, name, &t_new).expect("restore new");
        assert_eq!(
            std::fs::read(t_new.join("newsrc").join(name)).expect("restored"),
            std::fs::read(source.join(name)).expect("original")
        );
    }
    let report = verify(&repo, VerifyMode::Full, None, |_, _| Ok(())).expect("verify");
    assert!(report.ok(), "{:?}", report.issues);
    cleanup(&env);
}

/// Backend that refuses to write index files (crash between packs and
/// index).
struct NoIndexBackend {
    inner: LocalFs,
}

impl StorageBackend for NoIndexBackend {
    fn kind(&self) -> &'static str {
        "noindex"
    }
    fn describe(&self) -> String {
        self.inner.describe()
    }
    fn read(&self, rel: &str) -> rb_core::Result<Vec<u8>> {
        self.inner.read(rel)
    }
    fn read_range(&self, rel: &str, offset: u64, len: u64) -> rb_core::Result<Vec<u8>> {
        self.inner.read_range(rel, offset, len)
    }
    fn write_atomic(&self, rel: &str, data: &[u8]) -> rb_core::Result<()> {
        if rel.starts_with("index/") {
            return Err(rb_core::Error::Repo("injected crash before index".into()));
        }
        self.inner.write_atomic(rel, data)
    }
    fn exists(&self, rel: &str) -> rb_core::Result<bool> {
        self.inner.exists(rel)
    }
    fn mkdirs(&self, rel: &str) -> rb_core::Result<()> {
        self.inner.mkdirs(rel)
    }
    fn list(&self, prefix: &str) -> rb_core::Result<Vec<String>> {
        self.inner.list(prefix)
    }
    fn remove(&self, rel: &str) -> rb_core::Result<()> {
        self.inner.remove(rel)
    }
    fn open_writer(&self) -> rb_core::Result<Box<dyn ChunkWriter>> {
        self.inner.open_writer()
    }
}

/// P6: packs written but index missing: no snapshot, the next backup
/// succeeds, verify reports the unindexed pack, prune deletes it.
#[test]
fn p6_crash_between_packs_and_index_leaves_no_snapshot_and_prune_cleans_up() {
    let env = env("crash");
    let source = source_dir(&env, "src");
    std::fs::write(source.join("a.bin"), random_bytes(900_000, 1)).expect("a");
    let job = job(&env, "crash", &source);

    let crashing = Repository::open(
        Arc::new(NoIndexBackend {
            inner: LocalFs::new(&env.dest),
        }),
        PASS,
    )
    .expect("open");
    let err = run_backup(&crashing, &job, &BackupOptions::default(), |_| Ok(()))
        .expect_err("index write fails");
    assert!(err.to_string().contains("injected crash"), "{err}");
    assert!(files_in(&env.dest, "snapshots", ".snap").is_empty());
    assert!(files_in(&env.dest, "index", ".idx").is_empty());
    let orphan_packs = files_in(&env.dest, "packs", ".pack");
    assert_eq!(orphan_packs.len(), 1, "the pack landed, unindexed");

    // The next run succeeds (it simply re-stores the chunks).
    let (manifest, _) = run(&env.repo, &job);
    let report = verify(&env.repo, VerifyMode::Full, None, |_, _| Ok(())).expect("verify");
    assert!(report.ok(), "{:?}", report.issues);
    assert_eq!(report.unindexed_packs, 1, "reported separately");

    let prune = env.repo.prune_orphans(None, |_, _| {}).expect("prune");
    assert_eq!(prune.packs_deleted, 1, "unindexed pack deleted");
    assert!(
        !files_in(&env.dest, "packs", ".pack").contains(&orphan_packs[0]),
        "unindexed pack must be gone"
    );
    let report = verify(&env.repo, VerifyMode::Full, None, |_, _| Ok(())).expect("verify");
    assert!(report.ok() && report.unindexed_packs == 0);
    let t = env.base.join("t");
    restore_to(&env.repo, &manifest, &source, "a.bin", &t).expect("restore");
    assert_eq!(
        std::fs::read(t.join("src/a.bin")).expect("restored"),
        std::fs::read(source.join("a.bin")).expect("orig")
    );
    cleanup(&env);
}

/// P7: prune deletes fully dead packs, repacks a mostly dead pack and
/// rewrites the index; the remaining snapshot restores and verifies.
#[test]
fn p7_prune_deletes_dead_packs_repacks_mostly_dead_and_rewrites_index() {
    let env = env("prune");
    let keep_data = random_bytes(100_000, 11);
    let big_data = random_bytes(1_000_000, 12);

    // J1: small live file + big file in ONE pack.
    let s1 = source_dir(&env, "s1");
    std::fs::write(s1.join("keep.bin"), &keep_data).expect("keep");
    std::fs::write(s1.join("big.bin"), &big_data).expect("big");
    let (m1, _) = run(&env.repo, &job(&env, "j1", &s1));

    // J2: only the small file (fully deduplicated, no new pack).
    let s2 = source_dir(&env, "s2");
    std::fs::write(s2.join("keep.bin"), &keep_data).expect("keep2");
    let (m2, st2) = run(&env.repo, &job(&env, "j2", &s2));
    assert_eq!(st2.chunks_new, 0);

    // J3: unique file in its own pack, fully dead once its snapshot goes.
    let s3 = source_dir(&env, "s3");
    std::fs::write(s3.join("tmp.bin"), random_bytes(300_000, 13)).expect("tmp");
    let (m3, _) = run(&env.repo, &job(&env, "j3", &s3));
    assert_eq!(files_in(&env.dest, "packs", ".pack").len(), 2);

    env.repo.delete_snapshot(&m1.id).expect("delete s1");
    env.repo.delete_snapshot(&m3.id).expect("delete s3");
    let prune = env.repo.prune_orphans(None, |_, _| {}).expect("prune");
    assert_eq!(prune.packs_repacked, 1, "{prune:?}");
    assert_eq!(prune.packs_deleted, 2, "dead pack + repacked-away pack");
    assert_eq!(prune.index_files_rewritten, 1);
    assert!(prune.chunks_deleted > 0 && prune.bytes_freed > 0);
    assert_eq!(files_in(&env.dest, "packs", ".pack").len(), 1);
    assert_eq!(files_in(&env.dest, "index", ".idx").len(), 1);

    // A fresh repository instance reads only the rewritten index.
    let repo = Repository::open(Arc::new(LocalFs::new(&env.dest)), PASS).expect("open");
    let report = verify(&repo, VerifyMode::Full, None, |_, _| Ok(())).expect("verify");
    assert!(report.ok(), "{:?}", report.issues);
    assert_eq!(report.orphan_chunks, 0, "nothing dead survives the prune");
    let t = env.base.join("t");
    restore_to(&repo, &m2, &s2, "keep.bin", &t).expect("restore");
    assert_eq!(
        std::fs::read(t.join("s2/keep.bin")).expect("restored"),
        keep_data
    );
    let _ = m3;
    cleanup(&env);
}

/// P8: full verify names a corrupted blob inside a pack, and detects both a
/// damaged trailer and an index that drifted from the pack.
#[test]
fn p8_full_verify_detects_blob_corruption_trailer_damage_and_index_drift() {
    // 1. Corrupted byte inside a blob.
    let env1 = env("p8blob");
    let source = source_dir(&env1, "src");
    std::fs::write(source.join("a.bin"), random_bytes(700_000, 21)).expect("a");
    let (manifest, _) = run(&env1.repo, &job(&env1, "p8", &source));
    let first = manifest
        .iter_files()
        .find_map(|(_, e)| e.chunks.first().map(|c| c.hash))
        .expect("chunk");
    let (pack_rel, offset, length) = env1
        .repo
        .chunk_location(&first)
        .expect("loc")
        .expect("in a pack");
    let pack_path = env1
        .dest
        .join(pack_rel.replace('/', std::path::MAIN_SEPARATOR_STR));
    let mut bytes = std::fs::read(&pack_path).expect("pack");
    bytes[usize::try_from(offset + length / 2).expect("offset")] ^= 0x01;
    std::fs::write(&pack_path, &bytes).expect("corrupt");
    let report = verify(&env1.repo, VerifyMode::Full, None, |_, _| Ok(())).expect("verify");
    assert!(!report.ok());
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.message.contains(&first.to_hex()[..16])),
        "issue must name the chunk: {:?}",
        report.issues
    );
    let quick = verify(&env1.repo, VerifyMode::Quick, None, |_, _| Ok(())).expect("quick");
    assert!(quick.ok(), "quick checks presence only");
    cleanup(&env1);

    // 2. Damaged trailer.
    let env2 = env("p8trailer");
    let source = source_dir(&env2, "src");
    std::fs::write(source.join("a.bin"), random_bytes(300_000, 22)).expect("a");
    let (manifest, _) = run(&env2.repo, &job(&env2, "p8t", &source));
    let hash = manifest
        .iter_files()
        .find_map(|(_, e)| e.chunks.first().map(|c| c.hash))
        .expect("chunk");
    let (pack_rel, _, _) = env2.repo.chunk_location(&hash).expect("loc").expect("pack");
    let pack_path = env2
        .dest
        .join(pack_rel.replace('/', std::path::MAIN_SEPARATOR_STR));
    let mut bytes = std::fs::read(&pack_path).expect("pack");
    let tag_byte = bytes.len() - 5; // last byte of the sealed trailer
    bytes[tag_byte] ^= 0x01;
    std::fs::write(&pack_path, &bytes).expect("corrupt trailer");
    let report = verify(&env2.repo, VerifyMode::Full, None, |_, _| Ok(())).expect("verify");
    assert!(
        report.issues.iter().any(|i| i.message.contains("trailer")),
        "{:?}",
        report.issues
    );
    cleanup(&env2);

    // 3. Index drift: an index file claims a blob the pack does not hold.
    let env3 = env("p8drift");
    let source = source_dir(&env3, "src");
    std::fs::write(source.join("a.bin"), random_bytes(300_000, 23)).expect("a");
    let (manifest, _) = run(&env3.repo, &job(&env3, "p8d", &source));
    let hash = manifest
        .iter_files()
        .find_map(|(_, e)| e.chunks.first().map(|c| c.hash))
        .expect("chunk");
    let (pack_rel, _, _) = env3.repo.chunk_location(&hash).expect("loc").expect("pack");
    let pack_id = pack_id_of(&pack_rel).expect("pack id");
    env3.repo
        .write_index(&[IndexPack {
            pack: pack_id,
            blobs: vec![BlobEntry {
                hash: ChunkHash::from_data(b"bogus").to_hex(),
                offset: 5,
                length: 10,
                plain_len: 3,
            }],
        }])
        .expect("drifted index");
    let report = verify(&env3.repo, VerifyMode::Full, None, |_, _| Ok(())).expect("verify");
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.message.contains("index lists blob")),
        "{:?}",
        report.issues
    );
    cleanup(&env3);
}

/// Backend wrapper counting `read_range` / `read_ranges` / loose `read` calls.
struct CountingBackend {
    inner: LocalFs,
    ranges: std::sync::atomic::AtomicUsize,
    whole: std::sync::atomic::AtomicUsize,
}

impl CountingBackend {
    fn new(root: &Path) -> Arc<Self> {
        Arc::new(Self {
            inner: LocalFs::new(root),
            ranges: std::sync::atomic::AtomicUsize::new(0),
            whole: std::sync::atomic::AtomicUsize::new(0),
        })
    }
    fn range_calls(&self) -> usize {
        self.ranges.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn whole_reads(&self) -> usize {
        self.whole.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl StorageBackend for CountingBackend {
    fn open_writer(&self) -> rb_core::Result<Box<dyn ChunkWriter>> {
        self.inner.open_writer()
    }
    fn kind(&self) -> &'static str {
        self.inner.kind()
    }
    fn describe(&self) -> String {
        self.inner.describe()
    }
    fn read(&self, rel: &str) -> rb_core::Result<Vec<u8>> {
        if rel.starts_with("chunks/") {
            self.whole
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        self.inner.read(rel)
    }
    fn read_range(&self, rel: &str, offset: u64, len: u64) -> rb_core::Result<Vec<u8>> {
        self.ranges
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.read_range(rel, offset, len)
    }
    fn read_ranges(&self, rel: &str, ranges: &[(u64, u64)]) -> rb_core::Result<Vec<Vec<u8>>> {
        self.ranges
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.read_ranges(rel, ranges)
    }
    fn write_atomic(&self, rel: &str, data: &[u8]) -> rb_core::Result<()> {
        self.inner.write_atomic(rel, data)
    }
    fn exists(&self, rel: &str) -> rb_core::Result<bool> {
        self.inner.exists(rel)
    }
    fn list(&self, dir: &str) -> rb_core::Result<Vec<String>> {
        self.inner.list(dir)
    }
    fn remove(&self, rel: &str) -> rb_core::Result<()> {
        self.inner.remove(rel)
    }
    fn mkdirs(&self, rel: &str) -> rb_core::Result<()> {
        self.inner.mkdirs(rel)
    }
}

fn restore_leftovers(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.to_string_lossy().contains(".rbrestore-") {
                found.push(p);
            }
        }
    }
    found
}

/// Restore of ~600 small packed files with several workers is byte
/// identical and costs about one backend range read per pack touched.
#[test]
fn restore_many_small_files_reads_each_pack_once() {
    let env = env("window");
    let source = source_dir(&env, "src");
    std::fs::create_dir_all(source.join("d")).expect("d");
    let mut expected = Vec::new();
    for i in 0u64..600 {
        let rel = if i % 2 == 0 {
            format!("f{i:04}.bin")
        } else {
            format!("d/f{i:04}.bin")
        };
        let data = random_bytes(1500 + (i as usize % 11), i + 1);
        std::fs::write(source.join(&rel), &data).expect("file");
        expected.push((rel, data));
    }
    let (manifest, _) = run(&env.repo, &job(&env, "w", &source));
    let chunks: usize = manifest
        .sources
        .iter()
        .flat_map(|s| &s.entries)
        .map(|e| e.chunks.len())
        .sum();
    assert!(chunks >= 600);
    let packs = files_in(&env.dest, "packs", ".pack").len();

    let backend = CountingBackend::new(&env.dest);
    let repo = Repository::open(backend.clone(), PASS).expect("open");
    let target = env.base.join("out");
    let stats = restore_to(&repo, &manifest, &source, "", &target).expect("restore");
    assert_eq!(stats.files_restored, 600);
    let calls = backend.range_calls();
    assert!(
        calls <= packs + 2,
        "{calls} range reads for {packs} packs ({chunks} chunks)"
    );
    assert_eq!(backend.whole_reads(), 0, "no loose chunk read in a v2 repo");
    let root = target.join("src");
    for (rel, data) in &expected {
        assert_eq!(&std::fs::read(root.join(rel)).expect("restored"), data);
    }
    assert!(restore_leftovers(&target).is_empty());
    cleanup(&env);
}

/// Manifest with one source made of `entries` (restores need nothing else).
fn manifest_of(env: &Env, root: &Path, entries: Vec<FileEntry>) -> SnapshotManifest {
    let mut manifest = SnapshotManifest::new(Uuid::new_v4(), "fabricated");
    manifest.sources.push(SourceSnapshot {
        root: root.to_path_buf(),
        entries,
    });
    env.repo.write_manifest(&manifest).expect("manifest");
    manifest
}

fn file_entry(path: &str, chunks: Vec<ChunkRef>) -> FileEntry {
    FileEntry {
        path: PathBuf::from(path),
        size: chunks.iter().map(|c| c.plain_len).sum(),
        mtime_ns: 1_700_000_000_000_000_000,
        kind: EntryKind::File,
        chunks,
    }
}

/// A blob shared by two files and repeated several times inside one file is
/// read once and written at every offset.
#[test]
fn restore_shared_and_repeated_blobs_land_at_every_offset() {
    let env = env("shared");
    let source = source_dir(&env, "src");
    let data = random_bytes(900_000, 21);
    std::fs::write(source.join("x.bin"), &data).expect("x");
    std::fs::write(source.join("y.bin"), &data).expect("y");
    let (manifest, _) = run(&env.repo, &job(&env, "s", &source));
    let entries = &manifest.sources[0].entries;
    let x = entries
        .iter()
        .find(|e| e.path == Path::new("x.bin"))
        .expect("x entry");
    assert!(!x.chunks.is_empty());

    // rep.bin = x ++ x ++ first chunk of x (same blobs, several offsets).
    let mut rep_chunks = x.chunks.clone();
    rep_chunks.extend(x.chunks.clone());
    rep_chunks.push(x.chunks[0].clone());
    let first_len = x.chunks[0].plain_len as usize;
    let mut rep_expected = data.clone();
    rep_expected.extend_from_slice(&data);
    rep_expected.extend_from_slice(&data[..first_len]);
    let mut all = entries.clone();
    all.push(file_entry("rep.bin", rep_chunks));
    let fabricated = manifest_of(&env, &source, all);

    let backend = CountingBackend::new(&env.dest);
    let repo = Repository::open(backend.clone(), PASS).expect("open");
    let target = env.base.join("out");
    let stats = restore_to(&repo, &fabricated, &source, "", &target).expect("restore");
    assert_eq!(stats.files_restored, 3);
    let root = target.join("src");
    assert_eq!(std::fs::read(root.join("x.bin")).expect("x"), data);
    assert_eq!(std::fs::read(root.join("y.bin")).expect("y"), data);
    assert_eq!(
        std::fs::read(root.join("rep.bin")).expect("rep"),
        rep_expected
    );
    let packs = files_in(&env.dest, "packs", ".pack").len();
    assert!(backend.range_calls() <= packs + 2);
    assert!(restore_leftovers(&target).is_empty());
    cleanup(&env);
}

/// A file larger than a pack spans several packs; restored byte-identical
/// with one range read per pack, whatever the worker count.
#[test]
fn restore_large_file_spanning_several_packs() {
    let env = env("span");
    let source = source_dir(&env, "src");
    let big = random_bytes(36 * 1024 * 1024, 77);
    std::fs::write(source.join("big.bin"), &big).expect("big");
    std::fs::write(source.join("small.txt"), b"tail\n".repeat(100)).expect("small");
    let (manifest, _) = run(&env.repo, &job(&env, "span", &source));
    let packs = files_in(&env.dest, "packs", ".pack").len();
    assert!(packs >= 3, "expected several packs, got {packs}");

    let backend = CountingBackend::new(&env.dest);
    let repo = Repository::open(backend.clone(), PASS).expect("open");
    let target = env.base.join("out");
    let stats = restore_to(&repo, &manifest, &source, "", &target).expect("restore");
    assert_eq!(stats.files_restored, 2);
    assert!(
        std::fs::read(target.join("src/big.bin")).expect("big") == big,
        "big.bin differs"
    );
    assert!(backend.range_calls() <= packs + 2);
    assert!(restore_leftovers(&target).is_empty());
    cleanup(&env);
}

/// A repository holding only loose v1 chunks restores through the plan
/// engine too (shared and repeated loose chunks included).
#[test]
fn restore_from_loose_v1_chunks() {
    let env = env("loose");
    let root = source_dir(&env, "oldsrc");
    let parts: Vec<Vec<u8>> = (0u64..3)
        .map(|i| random_bytes(60_000 + i as usize, 30 + i))
        .collect();
    let refs: Vec<ChunkRef> = parts
        .iter()
        .map(|p| {
            let hash = ChunkHash::from_data(p);
            assert!(env.repo.store_chunk(&hash, p).expect("loose"));
            ChunkRef {
                hash,
                plain_len: p.len() as u64,
            }
        })
        .collect();
    let manifest = manifest_of(
        &env,
        &root,
        vec![
            file_entry(
                "a.bin",
                vec![refs[0].clone(), refs[1].clone(), refs[2].clone()],
            ),
            file_entry("b.bin", vec![refs[1].clone()]),
            file_entry(
                "c.bin",
                vec![refs[2].clone(), refs[2].clone(), refs[0].clone()],
            ),
        ],
    );
    assert!(env.repo.packed_chunk_hashes().expect("hashes").is_empty());

    let backend = CountingBackend::new(&env.dest);
    let repo = Repository::open(backend.clone(), PASS).expect("open");
    let target = env.base.join("out");
    let stats = restore_to(&repo, &manifest, &root, "", &target).expect("restore");
    assert_eq!(stats.files_restored, 3);
    let out = target.join("oldsrc");
    let cat = |ids: &[usize]| -> Vec<u8> { ids.iter().flat_map(|&i| parts[i].clone()).collect() };
    assert_eq!(
        std::fs::read(out.join("a.bin")).expect("a"),
        cat(&[0, 1, 2])
    );
    assert_eq!(std::fs::read(out.join("b.bin")).expect("b"), cat(&[1]));
    assert_eq!(
        std::fs::read(out.join("c.bin")).expect("c"),
        cat(&[2, 2, 0])
    );
    assert_eq!(backend.whole_reads(), 3, "each loose chunk is read once");
    assert!(restore_leftovers(&target).is_empty());
    cleanup(&env);
}

/// A restore cancelled mid-run reports `Cancelled` and leaves no temp file.
#[test]
fn cancelled_restore_leaves_no_temp_files() {
    let env = env("cancel");
    let source = source_dir(&env, "src");
    for i in 0u64..200 {
        std::fs::write(
            source.join(format!("f{i:03}.bin")),
            random_bytes(2000, i + 1),
        )
        .expect("file");
    }
    let (manifest, _) = run(&env.repo, &job(&env, "c", &source));
    let target = env.base.join("out");
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let options = RestoreOptions {
        cancel: Some(Arc::clone(&cancel)),
        ..RestoreOptions::default()
    };
    let mut seen = 0;
    let result = restore(
        &env.repo,
        &manifest.id,
        &[RestoreSelection {
            source_root: source.clone(),
            rel_path: PathBuf::new(),
        }],
        &RestoreDestination::Target(target.clone()),
        &options,
        |_| {
            seen += 1;
            if seen == 20 {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            Ok(())
        },
    );
    assert!(
        matches!(result, Err(rb_core::Error::Cancelled)),
        "{result:?}"
    );
    assert!(restore_leftovers(&target).is_empty());
    cleanup(&env);
}

/// Cancelling right after the first bytes were written (files in flight,
/// temp files open) removes every temp file.
#[test]
fn cancel_while_files_are_in_flight_cleans_up() {
    let env = env("cancel2");
    let source = source_dir(&env, "src");
    for i in 0u64..300 {
        std::fs::write(
            source.join(format!("g{i:03}.bin")),
            random_bytes(150_000, i + 1),
        )
        .expect("file");
    }
    let (manifest, _) = run(&env.repo, &job(&env, "c2", &source));
    let target = env.base.join("out");
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let options = RestoreOptions {
        cancel: Some(Arc::clone(&cancel)),
        ..RestoreOptions::default()
    };
    let result = restore(
        &env.repo,
        &manifest.id,
        &[RestoreSelection {
            source_root: source.clone(),
            rel_path: PathBuf::new(),
        }],
        &RestoreDestination::Target(target.clone()),
        &options,
        |event| {
            if matches!(event, rb_core::restore::RestoreEvent::BytesWritten { .. }) {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            Ok(())
        },
    );
    assert!(
        matches!(result, Err(rb_core::Error::Cancelled)),
        "{result:?}"
    );
    assert!(restore_leftovers(&target).is_empty());
    cleanup(&env);
}
