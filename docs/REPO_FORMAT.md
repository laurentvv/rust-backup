# Repository format v2 (v1 readable)

A repository is self-contained: restore and verification never need the local catalog.

```
<root>/
  config.json              public parameters (see below)
  keys/<key-id>.json       master key envelope (Argon2id + AES-256-GCM)
  chunks/<hh>/<hash>.chunk v1 loose chunk blobs, hash = BLAKE3(plaintext) (read forever, never written by v2 code)
  packs/<hh>/<pack-id>.pack v2 packs: many chunk blobs in one file (see below)
  index/<index-id>.idx     v2 sealed maps chunk hash -> (pack, offset, length)
  snapshots/<id>.snap      sealed snapshot manifests, written LAST (atomic)
  locks/<id>.json          exclusive write locks (pid, hostname, operation, timestamp)
```

## config.json

```json
{
  "version": 2,
  "chunker": { "min_size": 262144, "avg_size": 1048576, "max_size": 4194304 },
  "compression_level": 5,
  "created_at": "2026-11-07T12:00:00Z"
}
```

`version` is 2 for new repositories (packs + index). A version 1 repository (loose chunks only) stays readable and writable: its `config.json` is rewritten with `version: 2` (atomically) just before its first pack lands. Any other version is refused with `unsupported repository version N`, which is also what a v1-only binary does with a version 2 repository (it refuses instead of misreading it).

## keys/<id>.json

```json
{
  "algorithm": "AES-256-GCM + Argon2id",
  "kdf": { "m_cost": 65536, "t_cost": 3, "p_cost": 4 },
  "salt": "<32 hex>", "nonce": "<24 hex>", "ciphertext": "<hex>"
}
```

The master key (32 random bytes) is sealed with a key-encryption key derived from the user passphrase via Argon2id; associated data `rustbackup/masterkey/v1`. Wrong passphrase → authentication failure (no oracle beyond "wrong passphrase or corrupted key file").

## Chunk blob (`chunks/<first-2-hex>/<full-hash>.chunk`)

```
"RBCH" | version u8 (=1) | plain_len u64 LE | AES-256-GCM(nonce ‖ zstd(plaintext) ‖ tag)
```

Associated data = `BLAKE3(plaintext) ‖ plain_len` (both also enforced by the engine), so a swapped or truncated chunk fails authentication instead of restoring wrong data. Storing a chunk whose hash already exists is a no-op (dedup).

## Pack file (`packs/<first-2-hex>/<pack-id>.pack`)

```
"RBPK" | version u8 (=1) | blob_1 | ... | blob_n | trailer | trailer_len u32 LE
```

- `pack-id`: random 128-bit id, lowercase hex.
- Each `blob_i` is EXACTLY a v1 chunk blob (the `RBCH` layout above, same associated data), so reading a chunk from a pack is a byte-range read (`offset`, `length` from the index) followed by the v1 open path.
- `trailer` = `AES-256-GCM(nonce ‖ json ‖ tag)` with associated data `"RBPK" ‖ pack-id bytes`; the JSON is `{ "version": 1, "blobs": [ { "hash", "offset", "length", "plain_len" } ] }` (`offset` counted from the start of the pack file). It lets the index be rebuilt from packs alone and lets `verify --full` detect index/pack drift.
- Packs are built in memory by the upload writers and written whole at a size target (`RBACKUP_PACK_SIZE_MIB`, default 16, clamped 4..=128) and once more at the end of a run for the last partial pack.

## Index file (`index/<index-id>.idx`)

```
"RBIX" | version u8 (=1) | AES-256-GCM(nonce ‖ json ‖ tag)
```

Associated data = `"RBIX" ‖ index-id bytes`; JSON = `{ "version": 1, "packs": [ { "pack": "<pack-id>", "blobs": [ { "hash", "offset", "length", "plain_len" } ] } ] }`. One index file per backup run lists every pack that run wrote (runs that wrote no pack write none). The repository chunk map is the union of all index files (later files win on a duplicate hash; the content is identical anyway), loaded lazily once per opened repository.

## Snapshot manifest (`snapshots/<uuid>.snap`)

```
"RBSN" | version u8 (=1) | plain_len u64 LE | AES-256-GCM(nonce ‖ manifest JSON ‖ tag)
```

Associated data = `"RBSN" ‖ snapshot-uuid ‖ plain_len`. The JSON manifest (`version: 1`) contains: id, job id/name, creation time, host, kind (`full` or `incremental`), per-source entries (relative path, size, mtime ns, kind file/dir/symlink, ordered chunk refs `(hash, plain_len)`) and the skipped-files list with reasons.

## Atomicity

- Every file (chunk, manifest, lock) is written to a temp name containing `.tmp-<uuid>` in the same directory (`<stem>.tmp-<uuid>` on LocalFs, `<name>.tmp-<uuid>` over SFTP), **flushed to stable storage** (`sync_all`; `fsync@openssh.com` over SFTP when the server offers it), then renamed into place. A crash or a USB unplug never leaves a half-written file at its final name.
- **SFTP chunk uploads, batched durability** (default when the account may run commands): instead of one server `fsync` per chunk — the dominant cost on slow filesystems such as NTFS through ntfs-3g — each upload writer writes and closes chunks under their temp name **without** fsync, then per batch (256 chunks or 128 MiB) runs ONE remote `sync(2)` through a long-lived exec channel (a tiny `while read; do sync; echo ok; done` loop) and only then renames the batch into place. Before the manifest is written, every writer flushes its last batch and syncs once more so the renames are durable too. The invariant is unchanged: a chunk never appears under its final name before its bytes are on stable storage; a crash leaves only temp files (ignored by the chunk index, swept by `prune`). Accounts that cannot run commands (e.g. a key restricted with `command="internal-sftp"`) are detected at the first writer (no ready line within 10 s) and keep the per-chunk `fsync@openssh.com` path; `RBACKUP_SFTP_DURABILITY=fsync` forces that path. Manifests, keys, config and locks always use the per-file path.
- LocalFs renames over an existing target in one step (no window without the file). SFTP v3 cannot rename onto an existing name, so an overwrite there removes the target first; chunks and snapshot names are never overwritten in normal operation.
- Leftover temp files from interrupted writes are swept by `prune` (under the exclusive lock), under `chunks/`, `packs/`, `index/` and `snapshots/`.
- **Write order of a backup run** (v2): (1) every pack durable (writers `finish()`), (2) ONE index file, atomic and durable, (3) the snapshot manifest, last. A durable index therefore never points at a missing pack, and a snapshot never refers to a chunk the index cannot find. A crash after (1) leaves unindexed packs (invisible to readers, reported by `verify`, deleted by `prune`, their chunks re-stored by the next run); a crash after (2) leaves an index of valid packs and no snapshot.
- Snapshots: the manifest is created only after every chunk of the run is stored and indexed, then atomically renamed — an interrupted run leaves unreferenced chunks (harmless, reused later) but **never** a complete-looking snapshot.
- The SFTP batched-durability description above applies to packs (the unit of `write_new` in v2) exactly as it did to loose chunks.

## Paths

Repository-relative paths always use `/`; `LocalFs` maps them to the host separator, SFTP uses them verbatim under `remote_root`.

## Exclusive Locks (`locks/<uuid>.json`)

Write operations (`backup`, `prune`, `delete_snapshot`) acquire an exclusive lock:
```json
{
  "id": "<uuid>",
  "hostname": "workstation-1",
  "pid": 1234,
  "created_at": "2026-11-07T12:00:00Z",
  "operation": "backup"
}
```
Locks are automatically removed on completion or when the `RepositoryLock` guard drops. Stale locks (> 24 hours old) are broken automatically to recover from system or power failures.

## Snapshot Deletion & Pruning (Garbage Collection)

- **Snapshot Deletion**: Removes the sealed manifest file `snapshots/<id>.snap` from the repository and local tracking catalog.
- **Pruning** (exclusive lock): collects every chunk hash referenced by the remaining manifests, then
  1. unlinks unreferenced loose `.chunk` files (v1 behavior);
  2. classifies each indexed pack: fully referenced packs stay, fully unreferenced packs are deleted, and a partially referenced pack is **repacked** (live blobs copied by byte range into new packs, no re-encryption) when its unreferenced bytes are at least 50 % of the pack, otherwise kept as is;
  3. deletes unindexed packs (crash between pack write and index write) and temp files;
  4. commit order: new packs durable, then ONE consolidated index covering every surviving pack is written, then superseded index files are deleted, then dropped packs are deleted. A crash at any point leaves either the old or the new index set valid (extra index files only duplicate entries of packs that still exist).

  `PruneReport` carries `packs_deleted`, `packs_repacked` and `index_files_rewritten` next to the v1 counters.

## Verification

`verify` quick: every referenced chunk resolves (index entry whose pack file exists, or loose file). Full: additionally reads and authenticates every referenced chunk (byte-range read from its pack) and, for every pack, reads and opens its trailer and compares it with the index entries. Orphans (loose chunks or pack blobs no snapshot references) and unindexed packs are reported as informational counters.
