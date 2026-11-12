<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/brand/mark-dark.svg" />
    <img src="assets/brand/mark-light.svg" width="112" height="112" alt="RustBackup mark: an arch sheltering a brass hull" />
  </picture>
</p>

<h1 align="center">RustBackup</h1>

<p align="center">
  <strong>Encrypted, deduplicated backups for Windows — to a USB disk or a remote server over SFTP.</strong><br/>
  A native, GPU-rendered desktop app and a scriptable CLI, built on one Rust engine.
</p>

<p align="center">
  <a href="https://github.com/laurentvv/rust-backup/releases/latest"><img src="https://img.shields.io/github/v/release/laurentvv/rust-backup?color=82642B&label=release" alt="Latest release" /></a>
  <img src="https://img.shields.io/badge/platform-Windows_11_x64-0078D4?logo=windows" alt="Windows 11 x64" />
  <img src="https://img.shields.io/badge/Rust-2024_edition-orange?logo=rust" alt="Rust 2024 edition" />
  <img src="https://img.shields.io/badge/crypto-AES--256--GCM_%2B_Argon2id-14E0C8" alt="AES-256-GCM and Argon2id" />
  <img src="https://img.shields.io/badge/license-Apache--2.0-green" alt="Apache-2.0" />
</p>

<p align="center">
  <img src="assets/screenshots/rustbackup-walkthrough.gif" alt="RustBackup walkthrough: dashboard, backups, restore, history, settings and the dark theme" width="92%" />
</p>

---

## Why RustBackup

- **Your data, sealed before it leaves the machine.** Every chunk is compressed (zstd) and encrypted with AES-256-GCM; the master key is wrapped with Argon2id. The destination only ever sees ciphertext.
- **Self-contained repositories.** Restoring needs the repository and the passphrase — nothing else. The local catalog is a convenience, not a dependency.
- **Fast where it matters.** Content-defined chunking and a pack-file format make incremental runs of a 22 GiB, 419k-file tree finish in under a minute, and restores read each pack once with exact byte ranges.
- **Hard to break, safe by design.** Runs are atomic, sources are opened read-only and never modified, and a partial run can never look like a complete backup.

## Download

Get the latest build from the **[Releases page](https://github.com/laurentvv/rust-backup/releases/latest)**:

| File | What it is |
| :-- | :-- |
| `RustBackup-<version>-x64.msi` | Installer: desktop app + `rbackup` CLI, Start-menu shortcut, CLI added to `PATH` |
| `rust-backup.exe` | Portable desktop app |
| `rbackup.exe` | Portable command-line tool |

Requires Windows 11 (x64). The app is not code-signed yet, so SmartScreen may ask for confirmation on first launch.

## Features

**Backup**
- Full backup first, then incremental forever; a new full backup is scheduled automatically after *N* incrementals (default 10, per job, `0` disables) or forced on demand (`--full` / GUI toggle).
- FastCDC v2020 content-defined chunking (256 KiB / 1 MiB / 4 MiB) with BLAKE3 content addressing: an unchanged tree stores **zero new bytes**.
- Pack-file repository format (v2, ~16 MiB packs plus an index) — a few hundred files on the server instead of hundreds of thousands. Older v1 repositories stay readable.
- Local disks / USB drives and remote Linux servers over **SFTP** (`russh`, public-key auth, passphrase-protected keys, trust-on-first-use host-key pinning).
- Pipelined engine: one walker, parallel compress/seal workers, parallel upload writers on their own SSH connections, batched remote durability.

**Restore**
- Restore single files or whole subtrees to the original location or any folder, with skip/overwrite policy and exact modification times.
- Plan-driven: all needed chunks are resolved to pack ranges, each pack is read once, every chunk is authenticated and BLAKE3-checked, and files are written in parallel with live progress (files/s, MiB/s, time left).

**Integrity and maintenance**
- Quick verification (manifest authentication + chunk presence) and full verification (decrypt and re-hash every chunk).
- Snapshot deletion and orphan-chunk pruning, always behind an explicit confirmation; pruning only ever touches the backup destination.
- Cooperative repository locking with stale-lock recovery (`rbackup repo unlock`).

**Desktop app**
- Native GPU-rendered UI on [GPUI Kit](https://gpui-kit.com) with the *Arche* identity: limestone, ink and brass palettes in light and dark themes (WCAG 2.2 AA contrast checked by unit tests), Newsreader display serif.
- English and French interface, keyboard shortcuts (`Ctrl+1..4`, `Ctrl+B`, `Ctrl+,`), automatic catalog refresh, readable errors.

## Screenshots

| **Dashboard** | **Backup jobs (USB and SFTP)** |
| :---: | :---: |
| ![Dashboard](assets/screenshots/01-dashboard.png) | ![Backup jobs](assets/screenshots/02-jobs.png) |
| **Restore: snapshots and file tree** | **History and verification** |
| ![Restore](assets/screenshots/04-restore.png) | ![History](assets/screenshots/05-history.png) |
| **Settings** | **Dark theme** |
| ![Settings](assets/screenshots/06-settings.png) | ![Dark theme dashboard](assets/screenshots/08-dashboard-dark.png) |

<sub>Captured with a throwaway demo catalog. `RBACKUP_DB` points the GUI at any catalog file, like the CLI's `--db`.</sub>

## Performance

### Real-world run

Documents folder on a Windows 11 PC to a Linux server over SFTP (gigabit LAN, backup disk is a USB HDD mounted as NTFS through ntfs-3g):

| Run | Files | Source size | Stored | Time |
| :-- | --: | --: | --: | --: |
| Full | 418,882 | 22.3 GiB | 16.7 GiB (257,516 chunks) | **14 min 58 s** (25.4 MiB/s) |
| Incremental, next run | 418,876 | 22.3 GiB | **2 chunks (35 KB)** | **54.9 s** |
| Restore of a 11,443-file / 1.2 GiB folder | 11,443 | 1.2 GiB | — | **1 min 23 s**, byte-identical |

### Local USB drive

NVMe source to a USB 3.0 drive:

| Run | Files | Data | Time | New chunks |
| :-- | --: | --: | --: | :-- |
| Initial full | 6,875 | 215.6 MB | **26.8 s** | 6,416 (108 MB on disk) |
| Unchanged rerun | 6,875 | 215.6 MB | **0.43 s** | **0** |
| Incremental, 1 edit | 6,876 | 215.7 MB | **0.31 s** | 2 chunks (27 B) |
| Full verify | 6,418 chunks | 108 MB | **2.1 s** | 0 corruptions, 0 orphans |

### SFTP pipeline (synthetic corpus, same LAN server)

805 files, 1.04 GiB, destination on NTFS-via-FUSE:

| Engine | Upload writers | Wall time | Throughput |
| :-- | :-: | --: | --: |
| v1 sequential | 1 | 299 s | 3.5 MiB/s |
| v2 pipeline, per-chunk fsync | 4 | 127 s | 8.2 MiB/s |
| v2 pipeline, per-chunk fsync | 8 | 71 s | 14.7 MiB/s |
| **v2 pipeline, batched durability, one SSH connection per writer** | **8** | **64.5 s** | **16.1 MiB/s** |

Pack files cut the small-file case further: 4,000 files of 4–128 KiB go from 45.5 s to 25.8 s to **16.0 s** and land as 10 files on the server. The remaining ceiling on that box is the ntfs-3g daemon, not the client.

### SFTP restore

Full restore of each corpus, compared byte for byte with the source:

| Corpus | Per-chunk reads (before) | Plan-driven restore |
| :-- | --: | --: |
| 4,000 files, 4–128 KiB (256 MiB) | 105.4 s | **15.0 s** |
| 800 files, 8–32 KiB (16 MiB) | 22.0 s | **2.6 s** |
| Mixed: 4 × 256 MiB + 800 small files (1.04 GiB) | 254.2 s | **56.5 s** |

Tuning knobs (all optional): `RBACKUP_UPLOAD_WORKERS`, `RBACKUP_SFTP_WRITERS`, `RBACKUP_SFTP_READERS`, `RBACKUP_RESTORE_WORKERS`, `RBACKUP_SFTP_DURABILITY`, `RBACKUP_PROFILE=1`. Details in [docs/REPO_FORMAT.md](docs/REPO_FORMAT.md) and [docs/design/pack-format.md](docs/design/pack-format.md). Benchmarks are reproducible with `scripts/make_bench_corpus.py` and `scripts/bench_sftp.sh`.

## Command-line usage

The repository passphrase is read from the `RBACKUP_PASSPHRASE` environment variable — never from a flag, never logged.

```bash
export RBACKUP_PASSPHRASE="a long random passphrase"

# 1. Create an encrypted repository (USB disk, or a remote server over SFTP)
rbackup repo init --dest E:\Backups\vault
# rbackup repo init --sftp-host backup.example.com --sftp-user backup \
#     --sftp-key ~/.ssh/id_ed25519 --sftp-root /srv/backup

# 2. Define a job
rbackup jobs add --name work-docs --source C:\Users\me\Documents \
    --local-root E:\Backups\vault --max-incrementals 10

# 3. Back up (incremental, or full by cadence; --full forces one)
rbackup backup --job work-docs

# 4. List, verify, restore
rbackup snapshots --job work-docs
rbackup verify --job work-docs --full
rbackup restore --job work-docs --snapshot <snapshot-id> \
    --path Documents/project --to D:\Restored

# 5. Maintenance
rbackup delete-snapshot --job work-docs --snapshot <snapshot-id>
rbackup prune --job work-docs
```

## Build from source

Prerequisites: Windows 11 x64, a recent stable Rust toolchain (MSVC), Visual Studio Build Tools with the C++ workload, and CMake on `PATH`.

```bash
git clone https://github.com/laurentvv/rust-backup.git
cd rust-backup

cargo fmt --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked

cargo run --release --bin rust-backup     # desktop app
cargo run --release --bin rbackup -- --help
```

The MSI is built with the [WiX Toolset](https://wixtoolset.org) v5 from `installer/RustBackup.wxs`:

```bash
wix build installer/RustBackup.wxs -arch x64 -d Version=<version> \
    -d BinDir=target/release -o target/dist/RustBackup-<version>-x64.msi
```

## Architecture

```
rust-backup (Cargo workspace)
├── crates/rb-core     Engine library, no GUI dependency
│   ├── backend/       StorageBackend trait: LocalFs and SFTP
│   ├── chunker.rs     FastCDC v2020 chunker
│   ├── crypto.rs      Master key (zeroized), Argon2id wrap, AES-256-GCM
│   ├── pack.rs        Pack files and index (repository format v2)
│   ├── repo.rs        Repository: config, keys, packs, snapshots, locks
│   ├── backup.rs      Walk -> chunk -> dedup index -> parallel store
│   ├── restore.rs     Plan -> exact range reads -> verify -> atomic write
│   ├── verify.rs      Quick and full integrity verification
│   └── catalog.rs     Local DuckDB tracking catalog
├── crates/rb-cli      `rbackup` command-line tool
├── src/               Desktop app (GPUI Kit 0.9.0)
└── installer/         WiX source for the MSI
```

More in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), [docs/REPO_FORMAT.md](docs/REPO_FORMAT.md) and [docs/design/DESIGN_SYSTEM.md](docs/design/DESIGN_SYSTEM.md).

## Security

- **No secret leakage.** Passphrases and keys are never stored in clear, logged or put in error messages; the master key is zeroized on drop.
- **Tamper evidence.** Each chunk's associated data binds its BLAKE3 hash and plaintext length to the AEAD tag, so modifying any bit on the destination fails authentication.
- **Read-only sources.** The engine never modifies or deletes source data.
- **SSH host-key pinning.** Server keys are checked against a local `known_hosts` file (trust on first use); a changed key is refused.

Full threat model in [docs/SECURITY.md](docs/SECURITY.md).

## Roadmap

Retention policies, scheduled runs through the Windows Task Scheduler, optional passphrase storage in the Windows Credential Manager, tray icon and notifications, and Volume Shadow Copy for locked files. Not planned: cloud targets, a background daemon, file synchronization.

## License

Apache License 2.0 — see [LICENSE](LICENSE).
