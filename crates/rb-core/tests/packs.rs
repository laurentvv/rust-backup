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
