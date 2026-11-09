# Repository format v1

A repository is self-contained: restore and verification never need the local catalog.

```
<root>/
  config.json              public parameters (see below)
  keys/<key-id>.json       master key envelope (Argon2id + AES-256-GCM)
  chunks/<hh>/<hash>.chunk sealed chunk blobs, hash = BLAKE3(plaintext)
  snapshots/<id>.snap      sealed snapshot manifests, written LAST (atomic)
  locks/<id>.json          exclusive write locks (pid, hostname, operation, timestamp)
```

## config.json

```json
{
  "version": 1,
  "chunker": { "min_size": 262144, "avg_size": 1048576, "max_size": 4194304 },
  "compression_level": 5,
  "created_at": "2026-11-07T12:00:00Z"
}
```

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

## Snapshot manifest (`snapshots/<uuid>.snap`)

```
"RBSN" | version u8 (=1) | plain_len u64 LE | AES-256-GCM(nonce ‖ manifest JSON ‖ tag)
```

Associated data = `"RBSN" ‖ snapshot-uuid ‖ plain_len`. The JSON manifest (`version: 1`) contains: id, job id/name, creation time, host, kind (`full` or `incremental`), per-source entries (relative path, size, mtime ns, kind file/dir/symlink, ordered chunk refs `(hash, plain_len)`) and the skipped-files list with reasons.

## Atomicity

- Every file (chunk, manifest, lock) is written to a temp name containing `.tmp-<uuid>` in the same directory (`<stem>.tmp-<uuid>` on LocalFs, `<name>.tmp-<uuid>` over SFTP), **flushed to stable storage** (`sync_all`; `fsync@openssh.com` over SFTP when the server offers it), then renamed into place. A crash or a USB unplug never leaves a half-written file at its final name.
- **SFTP chunk uploads, batched durability** (default when the account may run commands): instead of one server `fsync` per chunk — the dominant cost on slow filesystems such as NTFS through ntfs-3g — each upload writer writes and closes chunks under their temp name **without** fsync, then per batch (256 chunks or 128 MiB) runs ONE remote `sync(2)` through a long-lived exec channel (a tiny `while read; do sync; echo ok; done` loop) and only then renames the batch into place. Before the manifest is written, every writer flushes its last batch and syncs once more so the renames are durable too. The invariant is unchanged: a chunk never appears under its final name before its bytes are on stable storage; a crash leaves only temp files (ignored by the chunk index, swept by `prune`). Accounts that cannot run commands (e.g. a key restricted with `command="internal-sftp"`) are detected at the first writer (no ready line within 10 s) and keep the per-chunk `fsync@openssh.com` path; `RBACKUP_SFTP_DURABILITY=fsync` forces that path. Manifests, keys, config and locks always use the per-file path.
- LocalFs renames over an existing target in one step (no window without the file). SFTP v3 cannot rename onto an existing name, so an overwrite there removes the target first; chunks and snapshot names are never overwritten in normal operation.
- Leftover temp files from interrupted writes are swept by `prune` (under the exclusive lock), under `chunks/` and `snapshots/`.
- Snapshots: the manifest is created only after every chunk of the run is stored, then atomically renamed — an interrupted run leaves unreferenced chunks (harmless, reused later) but **never** a complete-looking snapshot.

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
- **Pruning**: Traverses all remaining snapshot manifests, collects all referenced chunk hashes into an in-memory index, and unlinks unreferenced `.chunk` files from `chunks/`, reclaiming disk storage while guaranteeing all referenced data remains intact.
