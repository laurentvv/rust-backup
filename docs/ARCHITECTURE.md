# Architecture

## Workspace

```
rust-backup (GUI bin, GPUI Kit 0.9.0)
├── crates/rb-core      engine library (no UI deps except gpui-free core)
│   ├── backend/        StorageBackend trait + LocalFs + SftpBackend
│   ├── chunker.rs      FastCDC v2020 wrapper (config + bounds)
│   ├── hash.rs         ChunkHash (BLAKE3, hex, serde)
│   ├── compress.rs     zstd with strict exact-length decompress
│   ├── crypto.rs       MasterKey (zeroized), Argon2id wrap, AES-256-GCM chunk cipher
│   ├── repo.rs         repository format v1 (config/keys/chunks/snapshots)
│   ├── manifest.rs     SnapshotManifest (sources, entries, chunk refs, skipped)
│   ├── backup.rs       walk → chunk → dedup → store → manifest LAST
│   ├── restore.rs      plan → stream chunks (re-hashed) → atomic files → mtime
│   ├── verify.rs       quick/full integrity pass + orphans
│   └── catalog.rs      DuckDB migrations + jobs/runs/snapshots/verify CRUD
└── crates/rb-cli       rbackup: headless driver of the same engine
```

## Data flow (backup)

1. Walk each source (`walkdir`, sorted, symlinks recorded not followed).
2. Fast path: entries unchanged since the previous snapshot of the job (size + mtime ns) reuse their chunk list without reading.
3. Otherwise stream the file through `StreamCDC` (bounded memory), hash each chunk (BLAKE3), store it only if its hash is new (content-addressed dedup — this is what makes run #1 "full" and later runs "incremental").
4. Locked/unreadable files (Windows sharing violation) are skipped and recorded in the manifest.
5. The snapshot manifest is written and atomically renamed **last** — until then the snapshot does not exist for any reader.

## Backends

`StorageBackend` addresses the repository with `/`-separated relative paths; `LocalFs` (USB/internal) and `SftpBackend` (russh 0.65 + russh-sftp 3.0, public-key auth, Argon2-passphrase-protected keys supported, host keys pinned trust-on-first-use by SHA-256 fingerprint in a known-hosts file) implement it. SFTP runs on a private tokio runtime. All writes are temp file + flush to stable storage + rename (see REPO_FORMAT.md, Atomicity).

## Threading (GUI)

- Engine work runs on dedicated `std::thread`s (`src/runner.rs`); events stream over `flume` channels.
- The UI pumps the channel with `cx.spawn` + `recv_async`, applies events to the view state, `cx.notify()`.
- Cancellation is a cooperative `AtomicBool` checked between files.

## Catalog (DuckDB)

Local only, at `%LOCALAPPDATA%/RustBackup/catalog.duckdb` (migrations `0001_jobs_and_runs`, `0002_snapshots_and_verify`). Losing it never blocks a restore — the repository is self-contained.

## Testing

35 unit tests + 16 integration tests: chunker determinism/bounds/stability, crypto roundtrips and tamper rejection, full→incremental dedup with source-integrity hashes, cancelled-run leaves no manifest, Windows exclusive-lock skip, corruption detection naming the chunk, SFTP end-to-end against an in-process SSH/SFTP server (wrong-key rejection, passphrase-protected key, TOFU pinning).
