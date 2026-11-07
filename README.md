# RustBackup

Encrypted, deduplicated backups for Windows — USB disk or remote Linux server (SFTP, public-key auth) — with a GPU-accelerated native GUI (GPUI Kit).

- **Full backup, then incremental**: content-defined chunking (FastCDC v2020, 256 KiB / 1 MiB / 4 MiB) + BLAKE3 content addressing; a second run stores **zero** new data when nothing changed.
- **Compression + encryption (best practices)**: per-chunk zstd, then AES-256-GCM with the chunk hash and length as associated data (tamper/swap/truncation detection); the master key is wrapped with **Argon2id** (64 MiB, t=3, p=4).
- **Self-contained repository**: `config.json`, `keys/`, `chunks/`, `snapshots/` — restore never depends on the local database.
- **Partial restore**: any file or directory, to the original location or a destination of your choice, with skip/overwrite policy and mtime restoration.
- **Validity checks**: `verify` quick (existence + manifest authentication) and full (read + authenticate + re-hash every chunk); corruption is named by chunk hash.
- **Tracking database**: local DuckDB catalog (jobs, run history, snapshot cache, verification results).
- **Atomic by construction**: chunks and manifests land via temp-file + rename; an interrupted run never looks like a complete snapshot. Sources are never modified.
- **GUI**: French (default) / English, sidebar navigation, dashboard, live progress with cancel, restore browser, per-job verification.

## Layout

| Crate | Role |
| --- | --- |
| `crates/rb-core` | engine: chunker, crypto, repository, backends (local, SFTP), backup/restore/verify, DuckDB catalog |
| `crates/rb-cli` | `rbackup` headless CLI (jobs, repo init, backup, snapshots, restore, verify) |
| `rust-backup` | the GUI binary (GPUI Kit 0.9.0) |

## Quick start (CLI)

```bash
export RBACKUP_PASSPHRASE='a long random passphrase'
rbackup repo init --dest E:\Backups\depot           # or --sftp-host ... --sftp-user ... --sftp-key ... --sftp-root ...
rbackup jobs add --name documents --source C:\Users\me\Documents --local-root E:\Backups\depot
rbackup backup --job documents
rbackup verify --job documents --full
rbackup snapshots --job documents
rbackup restore --job documents --snapshot <id> --path Documents/notes.txt --to D:\Restore
```

## Docs

- `docs/ARCHITECTURE.md` — module map, data flow, threading model
- `docs/REPO_FORMAT.md` — on-disk repository format v1 (spec)
- `docs/SECURITY.md` — crypto design and threat model

Secrets are never passed as CLI arguments and never logged; the repository passphrase comes from `RBACKUP_PASSPHRASE`, the SSH key passphrase from `RBACKUP_KEY_PASSPHRASE` (GUI: session-only in-memory input).
