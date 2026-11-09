# Pack files — repository format v2 (PERF-3)

> Status: approved by the owner 2026-11-09 ("Yes, pack files"). Supersedes the "repository format v2 out of
> scope" note for this specific change (keyed chunk ids stay out of scope).
> Evidence: `.agent/scratch/bench-perf2-durability.md` — the reference destination (USB HDD behind ntfs-3g)
> accepts ~170 new files/s and ~60 MB/s sequential whatever the client, so one remote file per chunk is the
> ceiling. Prior art (restic design doc, kopia architecture page, borg internals; fetched 2026-11-09 from
> moving branches): packs of 16-40 MiB, per-pack trailer listing its blobs, separate index written after
> packs, manifest/commit last.

## Goals

1. New chunks are stored inside **pack files** (default target 16 MiB) instead of one file per chunk.
2. Per-file and per-directory restore keep reading **only the needed byte ranges**.
3. Crash consistency unchanged: a chunk is never referenced by a durable index before its pack is durable;
   a snapshot exists only once its manifest lands (manifest written last).
4. Existing v1 repositories (loose `chunks/` files) stay readable and writable; new writes go to packs.
5. Old binaries (v1-only) refuse a v2 repository with a clear error instead of misreading it.

## Layout (repo-relative)

```
config.json                      version 2 (new repos; v1 repos are upgraded on their first pack write)
keys/<id>.json                   unchanged
chunks/<2-hex>/<hash>.chunk      v1 loose chunks (read forever, never written by v2 code)
packs/<2-hex>/<pack-id>.pack     NEW
index/<index-id>.idx             NEW
snapshots/<id>.snap              unchanged, written LAST
locks/                           unchanged
```

`pack-id` and `index-id`: random 128-bit ids, lowercase hex (`Uuid::new_v4().simple()`), first two hex
chars as the shard directory for packs.

## Pack file

```
"RBPK" | version u8 (=1) | blob_1 | blob_2 | ... | blob_n | trailer | trailer_len u32 LE
```

- `blob_i` is EXACTLY the existing v1 chunk blob (`RBCH` | version | plain_len u64 LE | AES-GCM(...)),
  produced by `Repository::seal_chunk_blob` — same AD (chunk hash + plain_len). Reading a blob from a pack
  therefore reuses the v1 `unpack` + `cipher.open` path unchanged.
- `trailer` = `ChunkCipher::seal_with_ad(json(PackTrailer), ad = "RBPK" || pack-id bytes)` where
  `PackTrailer { version: 1, blobs: [ { hash: hex, offset: u64, length: u64, plain_len: u64 } ] }`
  (`offset` from the start of the pack file). The trailer lets the index be rebuilt from packs alone.

## Index file

`index/<index-id>.idx` = `"RBIX" | version u8 (=1) | seal_with_ad(json(IndexFile), ad = "RBIX" || index-id)`
with `IndexFile { version: 1, packs: [ { pack: pack-id, blobs: [ { hash, offset, length, plain_len } ] } ] }`.

- One index file per backup run, listing every pack that run wrote; written with `write_atomic` (durable)
  AFTER every pack writer's `finish()` returned Ok, and BEFORE the manifest.
- The repository's chunk map = union of all index files (later files win on duplicate hashes — identical
  content anyway). Loaded lazily, once per `Repository` instance (shared across clones).
- Runs that wrote no pack write no index file.

## Write path (backup)

- Seal workers unchanged (they produce v1 chunk blobs).
- Each upload writer owns a pack builder: append blobs to an in-memory buffer; when the buffer reaches the
  target size (`RBACKUP_PACK_SIZE_MIB`, default 16, clamp 4..=128) — or at `finish()` — seal the trailer,
  write the whole pack with `ChunkWriter::write_new("packs/<xx>/<id>.pack", bytes)` (same durability
  contract: temp name, batched/fsync durability, rename), and record its blob entries.
- Pipeline `finish()`: all writers finish (last partial pack written + made durable) → collect entries →
  write ONE index file → (caller) write manifest.
- In-run dedup and the run-start dedup set = loose chunk hashes (listing `chunks/`) ∪ chunk-map hashes.
  Listing `chunks/` on a v2-only repository is cheap (empty dir); keep it for mixed repos.
- First pack write into a v1 repository rewrites `config.json` with `version: 2` (atomic) before the first
  pack lands.

## Read path

- `read_chunk(hash, plain_len)`: chunk map hit → `backend.read_range(pack_rel, offset, length)` → existing
  blob checks (magic, plain_len) → `cipher.open` → decompress; miss → v1 loose file (current behavior).
- New `StorageBackend::read_range(rel, offset, len) -> Result<Vec<u8>>` (LocalFs: open + seek + read_exact;
  SFTP: open + seek + read_exact on the shared session). Error if the file is shorter than requested.
- `chunk_exists(hash)`: chunk map hit (and the pack is listed) or loose file exists.

## Verify

- Quick: every referenced chunk resolvable (map entry whose pack file exists, or loose file).
- Full: additionally `read_chunk` every referenced chunk (MAC + plaintext BLAKE3 as today) AND, for every
  pack, read + open its trailer and check it matches the index entries (detects index/pack drift).
- Orphans (informational): loose chunks + pack blobs not referenced by any snapshot; packs present on disk
  but in no index ("unindexed packs") are reported separately.

## Prune (exclusive lock held)

1. Referenced set from all manifests (as today).
2. Loose chunks: unchanged behavior (delete unreferenced, sweep temp files).
3. Packs: classify each indexed pack — fully referenced (keep), fully unreferenced (delete), partially
   referenced: repack when unreferenced bytes ≥ 50 % of the pack (read referenced blobs by range, write them
   into new packs through a writer, `finish()`), else keep as is.
4. Unindexed packs (crash between pack write and index write) and pack temp files: delete.
5. Commit order: new packs durable → write ONE new consolidated index covering every surviving pack →
   delete superseded index files → delete dropped/repacked packs. A crash at any point leaves either the old
   or the new index set valid (extra index files only add duplicate entries for packs that still exist).
6. `PruneReport` gains `packs_deleted`, `packs_repacked`, `index_files_rewritten` (serde default 0).

## Acceptance criteria (each backed by a test unless stated)

| # | Criterion | Check |
|---|---|---|
| P1 | Backup of 1 200 small files on LocalFs creates ≤ 2 pack files (16 MiB target) and 0 loose chunks | test |
| P2 | Single-file and directory restore from packs are byte-identical | test (local + SFTP in-process) |
| P3 | `read_range` reads only the requested bytes (LocalFs + SFTP) and errors on short files | test |
| P4 | Mixed repo: v1 loose chunks + new packs in one repository; old snapshots and new snapshots restore | test |
| P5 | A v2 `config.json` is refused by the version check of a v1-only reader (message names the version) | test on `Repository::open` with version 3 + doc note |
| P6 | Crash after packs but before index: no snapshot, next backup succeeds, prune deletes the unindexed packs | test (inject failure) |
| P7 | Prune: deleting a snapshot then pruning deletes fully unreferenced packs, repacks a ≥50 % dead pack, rewrites the index; remaining snapshots restore and full-verify OK | test |
| P8 | Full verify detects a corrupted byte inside a pack blob and a trailer/index mismatch | test |
| P9 | Workspace gate green (fmt, clippy -D warnings, all tests) | gate |
| P10 | Real server bench (documents + mixed corpora) faster than PERF-2 (25.8 s / 64.5 s) | `scripts/bench_sftp.sh` |
| P11 | Real server end-to-end: file + directory restore byte-identical, full verify OK | `scripts/e2e_sftp_restore.sh` |
