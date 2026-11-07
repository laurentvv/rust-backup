//! End-to-end restore and verification tests on the local path.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rb_core::backend::{LocalFs, StorageBackend};
use rb_core::backup::{BackupOptions, run_backup};
use rb_core::chunker::ChunkerConfig;
use rb_core::crypto::KdfParams;
use rb_core::domain::{Destination, JobConfig};
use rb_core::hash::ChunkHash;
use rb_core::manifest::SnapshotManifest;
use rb_core::repo::Repository;
use rb_core::restore::{
    OverwritePolicy, RestoreDestination, RestoreOptions, RestoreSelection, restore,
};
use rb_core::verify::{VerifyMode, verify};
use uuid::Uuid;

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

struct Fixture {
    base: PathBuf,
    source: PathBuf,
    dest: PathBuf,
    job: JobConfig,
    repo: Repository,
}

fn fixture(tag: &str) -> Fixture {
    let base = std::env::temp_dir().join(format!("rb-rv-{tag}-{}", Uuid::new_v4()));
    let source = base.join("src");
    let dest = base.join("repo");
    std::fs::create_dir_all(source.join("nested")).expect("dirs");
    std::fs::write(source.join("a.bin"), random_bytes(2_600_000, 7)).expect("a.bin");
    std::fs::write(source.join("nested/b.bin"), random_bytes(900_000, 8)).expect("b.bin");
    std::fs::write(
        source.join("nested/c.txt"),
        b"configuration payload\n".repeat(100),
    )
    .expect("c.txt");
    std::fs::write(source.join("empty.txt"), b"").expect("empty.txt");

    let job = JobConfig {
        id: Uuid::new_v4(),
        name: format!("rv-{tag}"),
        sources: vec![source.clone()],
        destination: Destination::Local { root: dest.clone() },
        compression_level: 5,
        created_at: chrono::Utc::now(),
    };
    let backend: Arc<dyn StorageBackend> = Arc::new(LocalFs::new(&dest));
    let repo = Repository::init(
        backend,
        "restore passphrase",
        ChunkerConfig::default(),
        5,
        fast_kdf(),
    )
    .expect("init");
    Fixture {
        base,
        source,
        dest,
        job,
        repo,
    }
}

fn backup(fixture: &Fixture) -> SnapshotManifest {
    let (manifest, _stats) = run_backup(
        &fixture.repo,
        &fixture.job,
        &BackupOptions::default(),
        |_| Ok(()),
    )
    .expect("backup");
    manifest
}

fn select(fixture: &Fixture, rel: &str) -> Vec<RestoreSelection> {
    vec![RestoreSelection {
        source_root: fixture.source.clone(),
        rel_path: PathBuf::from(rel),
    }]
}

fn mtime_secs(path: &Path) -> i64 {
    path.metadata()
        .expect("metadata")
        .modified()
        .expect("mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("positive")
        .as_secs() as i64
}

#[test]
fn restore_single_file_to_chosen_destination_is_byte_identical() {
    let fixture = fixture("onefile");
    let manifest = backup(&fixture);
    let target = fixture.base.join("target");

    let stats = restore(
        &fixture.repo,
        &manifest.id,
        &select(&fixture, "nested/c.txt"),
        &RestoreDestination::Target(target.clone()),
        &RestoreOptions::default(),
        |_| Ok(()),
    )
    .expect("restore");

    assert_eq!(stats.files_restored, 1);
    let restored = target
        .join(fixture.source.file_name().expect("name"))
        .join("nested/c.txt");
    assert_eq!(
        std::fs::read(&restored).expect("read restored"),
        std::fs::read(fixture.source.join("nested/c.txt")).expect("read source"),
        "restored file must be byte-identical"
    );
}

#[test]
fn restore_directory_to_original_location_after_damage() {
    let fixture = fixture("damage");
    let original_c = fixture.source.join("nested/c.txt");
    let original_bytes = std::fs::read(&original_c).expect("read c");
    let original_mtime = mtime_secs(&original_c);
    let manifest = backup(&fixture);

    // Damage the source: delete one file, corrupt another, drop the dir.
    std::fs::remove_file(&original_c).expect("delete c");
    let mut b = std::fs::read(fixture.source.join("nested/b.bin")).expect("read b");
    b[..64].copy_from_slice(&[0u8; 64]);
    std::fs::write(fixture.source.join("nested/b.bin"), &b).expect("corrupt b");
    std::fs::remove_file(fixture.source.join("a.bin")).expect("delete a");

    let stats = restore(
        &fixture.repo,
        &manifest.id,
        &select(&fixture, "nested"),
        &RestoreDestination::Original,
        &RestoreOptions {
            overwrite: OverwritePolicy::Overwrite,
            cancel: None,
        },
        |_| Ok(()),
    )
    .expect("restore nested to original");

    assert_eq!(
        std::fs::read(&original_c).expect("reread c"),
        original_bytes
    );
    assert!(
        (mtime_secs(&original_c) - original_mtime).abs() <= 2,
        "mtime must be restored (got {}, want ~{original_mtime})",
        mtime_secs(&original_c)
    );
    assert!(
        !fixture.source.join("a.bin").exists(),
        "a.bin outside selection"
    );
    let restored_b = std::fs::read(fixture.source.join("nested/b.bin")).expect("b");
    assert!(restored_b[..64] != [0u8; 64], "corrupted b must be healed");
    let _ = stats;
}

#[test]
fn overwrite_policy_skip_keeps_existing_overwrite_replaces() {
    let fixture = fixture("policy");
    let manifest = backup(&fixture);
    let target = fixture.base.join("target");
    let dest_c = target
        .join(fixture.source.file_name().expect("name"))
        .join("nested/c.txt");
    std::fs::create_dir_all(dest_c.parent().expect("parent")).expect("dirs");
    std::fs::write(&dest_c, b"NEW LOCAL CONTENT").expect("pre-existing");

    // Skip (default): the local file survives.
    restore(
        &fixture.repo,
        &manifest.id,
        &select(&fixture, "nested/c.txt"),
        &RestoreDestination::Target(target.clone()),
        &RestoreOptions::default(),
        |_| Ok(()),
    )
    .expect("restore skip");
    assert_eq!(
        std::fs::read(&dest_c).expect("read"),
        b"NEW LOCAL CONTENT",
        "skip policy must keep the existing file"
    );

    // Overwrite: the snapshot version replaces it.
    restore(
        &fixture.repo,
        &manifest.id,
        &select(&fixture, "nested/c.txt"),
        &RestoreDestination::Target(target.clone()),
        &RestoreOptions {
            overwrite: OverwritePolicy::Overwrite,
            cancel: None,
        },
        |_| Ok(()),
    )
    .expect("restore overwrite");
    assert_eq!(
        std::fs::read(&dest_c).expect("read"),
        std::fs::read(fixture.source.join("nested/c.txt")).expect("src"),
        "overwrite policy must restore the snapshot version"
    );
}

#[test]
fn restore_selection_matching_nothing_fails_cleanly() {
    let fixture = fixture("nomatch");
    let manifest = backup(&fixture);
    let err = restore(
        &fixture.repo,
        &manifest.id,
        &select(&fixture, "does/not/exist"),
        &RestoreDestination::Target(fixture.base.join("t")),
        &RestoreOptions::default(),
        |_| Ok(()),
    )
    .unwrap_err();
    assert!(matches!(err, rb_core::Error::Repo(_)), "got {err:?}");
}

#[test]
fn verify_quick_passes_then_full_detects_and_names_corruption() {
    let fixture = fixture("corrupt");
    let manifest = backup(&fixture);

    // Healthy repository: both modes pass.
    let quick =
        verify(&fixture.repo, VerifyMode::Quick, None, |_, _| Ok(())).expect("verify quick");
    assert!(quick.ok(), "quick must pass on healthy repo");
    assert!(quick.manifests_checked >= 1);
    assert!(quick.chunks_checked >= 3);

    let full = verify(&fixture.repo, VerifyMode::Full, None, |_, _| Ok(())).expect("verify full");
    assert!(full.ok(), "full must pass on healthy repo");

    // Corrupt one stored chunk (flip a byte in the middle of a chunk file).
    let chunk_files = {
        let backend = LocalFs::new(&fixture.dest);
        backend.list("chunks").expect("chunks")
    };
    assert!(!chunk_files.is_empty());
    let victim = fixture.dest.join(
        chunk_files
            .first()
            .expect("victim")
            .replace('/', std::path::MAIN_SEPARATOR_STR),
    );
    let victim_name = chunk_files[0]
        .rsplit('/')
        .next()
        .expect("name")
        .trim_end_matches(".chunk")
        .to_string();
    let mut blob = std::fs::read(&victim).expect("read chunk");
    let mid = blob.len() / 2;
    blob[mid] ^= 0x40;
    std::fs::write(&victim, &blob).expect("corrupt chunk");

    let corrupted = verify(&fixture.repo, VerifyMode::Full, None, |_, _| Ok(()))
        .expect("verify full on corrupted");
    assert!(!corrupted.ok(), "full must FAIL on corrupted chunk");
    assert!(
        corrupted
            .issues
            .iter()
            .any(|i| i.message.contains(&victim_name[..16])),
        "issue must name the corrupted chunk hash, got {:?}",
        corrupted.issues
    );

    // Quick mode only checks presence: the file is still there.
    let quick2 =
        verify(&fixture.repo, VerifyMode::Quick, None, |_, _| Ok(())).expect("verify quick 2");
    assert!(quick2.ok(), "quick checks existence only (by design)");

    // And the restore of the file using that chunk must fail cleanly.
    let victim_file = manifest
        .iter_files()
        .find(|(_, e)| e.chunks.iter().any(|c| c.hash.to_hex() == victim_name))
        .map(|(_, e)| e.path.to_string_lossy().to_string());
    if let Some(rel) = victim_file {
        let err = restore(
            &fixture.repo,
            &manifest.id,
            &select(&fixture, &rel),
            &RestoreDestination::Target(fixture.base.join("t2")),
            &RestoreOptions {
                overwrite: OverwritePolicy::Overwrite,
                cancel: None,
            },
            |_| Ok(()),
        );
        assert!(err.is_err(), "restoring a corrupted chunk must fail");
        // No half-written file left at the final name.
        let final_path = fixture
            .base
            .join("t2")
            .join(fixture.source.file_name().expect("name"))
            .join(&rel);
        assert!(
            !final_path.exists(),
            "failed restore must leave no final file"
        );
    }
}

#[test]
fn restore_of_empty_file_and_full_snapshot_works() {
    let fixture = fixture("whole");
    let manifest = backup(&fixture);
    let target = fixture.base.join("target");

    // Whole-snapshot restore into a fresh target (empty rel path = all).
    let stats = restore(
        &fixture.repo,
        &manifest.id,
        &select(&fixture, ""),
        &RestoreDestination::Target(target.clone()),
        &RestoreOptions::default(),
        |_| Ok(()),
    )
    .expect("restore whole");
    assert_eq!(stats.files_restored, 4);

    let base = target.join(fixture.source.file_name().expect("name"));
    assert_eq!(std::fs::read(base.join("empty.txt")).expect("empty"), b"");
    assert_eq!(
        std::fs::read(base.join("a.bin")).expect("a"),
        std::fs::read(fixture.source.join("a.bin")).expect("src a")
    );
    let restored_hash = ChunkHash::from_data(&std::fs::read(base.join("nested/b.bin")).expect("b"));
    let source_hash =
        ChunkHash::from_data(&std::fs::read(fixture.source.join("nested/b.bin")).expect("src b"));
    assert_eq!(restored_hash, source_hash);
}
