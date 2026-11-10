//! SFTP backend: repository on a remote Linux server, public-key auth.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use russh_sftp::client::SftpSession;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{ChunkWriter, StorageBackend};

/// How the server's host key is checked during connection.
#[derive(Debug, Clone)]
pub enum HostKeyPolicy {
    /// Accept any host key. TESTS ONLY — never expose in the GUI.
    AcceptAll,
    /// Trust on first use: remember `sha256(fingerprint)` per host in a
    /// known-hosts file and refuse silent changes (interception guard).
    TrustOnFirstUse {
        /// Path of the known-hosts file (created on first connect).
        known_hosts: PathBuf,
    },
}

/// Connection parameters for [`SftpBackend::connect`].
#[derive(Debug, Clone)]
pub struct SftpConnect {
    /// Remote host name or address.
    pub host: String,
    /// SSH port.
    pub port: u16,
    /// SSH user name.
    pub username: String,
    /// Path of the private key file (OpenSSH/PEM; passphrase-supported).
    pub key_path: PathBuf,
    /// Passphrase of the private key, when encrypted.
    pub key_passphrase: Option<String>,
    /// Repository root directory on the remote host.
    pub remote_root: String,
    /// Host key verification policy.
    pub host_key_policy: HostKeyPolicy,
}

/// Storage on a remote SFTP server. All operations run on a private tokio
/// runtime; the [`StorageBackend`] surface stays synchronous for the engine.
#[derive(Clone)]
pub struct SftpBackend {
    runtime: Arc<tokio::runtime::Runtime>,
    sftp: Arc<SftpSession>,
    /// Live SSH connection (control SFTP channel + durability barrier).
    session: Arc<russh::client::Handle<ClientHandler>>,
    /// Connection parameters, kept to open one extra SSH connection per
    /// parallel chunk writer (see [`WriterTransport`]).
    params: Arc<SftpConnect>,
    /// Remote durability barrier shared by every chunk writer, probed once
    /// on the first `open_writer` (`None` = per-chunk fsync fallback).
    barrier: Arc<OnceLock<Option<Arc<RemoteBarrier>>>>,
    remote_root: String,
    description: String,
}

impl std::fmt::Debug for SftpBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SftpBackend")
            .field("remote_root", &self.remote_root)
            .finish()
    }
}

impl SftpBackend {
    /// Connects, authenticates with the public key, opens the SFTP
    /// subsystem and makes sure the repository root exists.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] for transport failures,
    /// [`Error::Crypto`]-style messages wrapped in [`Error::Repo`] for auth
    /// and host-key failures.
    pub fn connect(params: &SftpConnect) -> crate::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .map_err(|e| crate::Error::Io(std::io::Error::other(e.to_string())))?;

        let (sftp, session) = runtime.block_on(connect_inner(params))?;
        Ok(Self {
            runtime: Arc::new(runtime),
            sftp: Arc::new(sftp),
            session: Arc::new(session),
            params: Arc::new(params.clone()),
            barrier: Arc::new(OnceLock::new()),
            remote_root: params.remote_root.clone(),
            description: format!(
                "sftp: {}@{}:{}",
                params.username, params.host, params.remote_root
            ),
        })
    }

    /// Shuts the SSH session down cleanly (optional: dropping works too).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Repo`] when the goodbye round-trip fails.
    pub fn close(&self) -> crate::Result<()> {
        let sftp = Arc::clone(&self.sftp);
        self.runtime.block_on(async move { sftp.close().await })?;
        Ok(())
    }

    /// Chunk durability mode used by the parallel writers of this
    /// connection: `"batched-sync"` (remote `sync` barrier per batch) or
    /// `"per-chunk-fsync"`. Probes the server on first call.
    #[must_use]
    pub fn chunk_durability(&self) -> &'static str {
        if self.barrier().is_some() {
            "batched-sync"
        } else {
            "per-chunk-fsync"
        }
    }

    fn barrier(&self) -> Option<Arc<RemoteBarrier>> {
        self.barrier
            .get_or_init(|| {
                if std::env::var(DURABILITY_ENV).is_ok_and(|v| v.eq_ignore_ascii_case("fsync")) {
                    return None;
                }
                let session = Arc::clone(&self.session);
                self.runtime
                    .block_on(async move { RemoteBarrier::open(&session).await })
                    .map(Arc::new)
            })
            .clone()
    }

    fn remote_path(&self, rel: &str) -> String {
        format!(
            "{}/{}",
            self.remote_root.trim_end_matches('/'),
            rel.trim_start_matches('/')
        )
    }
}

impl StorageBackend for SftpBackend {
    fn kind(&self) -> &'static str {
        "sftp"
    }

    fn describe(&self) -> String {
        self.description.clone()
    }

    fn read(&self, rel: &str) -> crate::Result<Vec<u8>> {
        let path = self.remote_path(rel);
        let sftp = Arc::clone(&self.sftp);
        self.runtime.block_on(async move {
            let mut file = sftp
                .open_with_flags(&path, russh_sftp::protocol::OpenFlags::READ)
                .await?;
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).await?;
            file.shutdown().await?;
            Ok(buf)
        })
    }

    fn write_atomic(&self, rel: &str, data: &[u8]) -> crate::Result<()> {
        let path = self.remote_path(rel);
        let tmp = format!("{path}.tmp-{}", uuid::Uuid::new_v4().simple());
        let sftp = Arc::clone(&self.sftp);
        let data = data.to_vec();
        self.runtime.block_on(async move {
            // Ensure parent dirs exist BEFORE opening the temp file.
            if let Some(parent) = parent_of(&path) {
                mkdirs_remote(&sftp, &parent).await?;
            }
            // Write the full content under a temp name and flush it to the
            // server's disk (`fsync@openssh.com`; russh-sftp turns this into a
            // no-op when the server lacks the extension).
            let written: crate::Result<()> = async {
                let mut file = sftp
                    .open_with_flags(
                        &tmp,
                        russh_sftp::protocol::OpenFlags::CREATE
                            | russh_sftp::protocol::OpenFlags::WRITE
                            | russh_sftp::protocol::OpenFlags::TRUNCATE,
                    )
                    .await?;
                file.write_all(&data).await?;
                file.sync_all().await?;
                file.shutdown().await?;
                // SFTP v3 RENAME refuses an existing target and russh-sftp has
                // no `posix-rename@openssh.com`, so an overwrite must remove
                // first. Content-addressed chunks and fresh snapshot names are
                // never overwritten in normal operation, so this window only
                // concerns explicit rewrites.
                if sftp.metadata(&path).await.is_ok() {
                    sftp.remove_file(&path).await?;
                }
                sftp.rename(&tmp, &path).await?;
                Ok(())
            }
            .await;
            if written.is_err() {
                let _ = sftp.remove_file(&tmp).await;
            }
            written
        })
    }

    fn exists(&self, rel: &str) -> crate::Result<bool> {
        let path = self.remote_path(rel);
        let sftp = Arc::clone(&self.sftp);
        self.runtime.block_on(async move {
            match sftp.metadata(&path).await {
                Ok(_) => Ok(true),
                // Only a definite "no such file" means absent; a dropped
                // connection or timeout must not read as "absent".
                Err(russh_sftp::client::error::Error::Status(status))
                    if status.status_code == russh_sftp::protocol::StatusCode::NoSuchFile =>
                {
                    Ok(false)
                }
                Err(err) => Err(err.into()),
            }
        })
    }

    fn mkdirs(&self, rel: &str) -> crate::Result<()> {
        let path = self.remote_path(rel);
        let sftp = Arc::clone(&self.sftp);
        self.runtime
            .block_on(async move { mkdirs_remote(&sftp, &path).await })
    }

    fn list(&self, prefix: &str) -> crate::Result<Vec<String>> {
        let base = self.remote_path(prefix);
        let sftp = Arc::clone(&self.sftp);
        let prefix = prefix.trim_matches('/').to_string();
        let entries = self.runtime.block_on(async move {
            if sftp.metadata(&base).await.is_err() {
                return Ok(Vec::new());
            }
            walk_remote(&sftp, &base, &prefix).await
        })?;
        let mut sorted = entries;
        sorted.sort();
        Ok(sorted)
    }

    fn remove(&self, rel: &str) -> crate::Result<()> {
        let path = self.remote_path(rel);
        let sftp = Arc::clone(&self.sftp);
        self.runtime.block_on(async move {
            if sftp.metadata(&path).await.is_ok() {
                sftp.remove_file(&path).await?;
            }
            Ok(())
        })
    }

    fn open_writer(&self) -> crate::Result<Box<dyn ChunkWriter>> {
        // Probe (once) before opening the writer channel so the barrier
        // channel gets its slot on the shared connection.
        let barrier = self.barrier();
        let (sftp, connection) = match WriterTransport::from_env() {
            WriterTransport::Connection => {
                let params = Arc::clone(&self.params);
                let (sftp, session) = self
                    .runtime
                    .block_on(async move { connect_inner(&params).await })?;
                (sftp, Some(session))
            }
            WriterTransport::Channel => {
                let session = Arc::clone(&self.session);
                let sftp = self
                    .runtime
                    .block_on(async { open_sftp_channel(&session).await })?;
                (sftp, None)
            }
        };
        Ok(Box::new(SftpChunkWriter {
            runtime: Arc::clone(&self.runtime),
            sftp: Arc::new(sftp),
            _connection: connection,
            remote_root: self.remote_root.clone(),
            dirs: HashSet::new(),
            barrier,
            pending: Vec::new(),
            pending_bytes: 0,
            renamed_since_sync: false,
            op_times: OpTimes::default(),
        }))
    }
}

/// How each parallel chunk writer reaches the server.
///
/// Default **connection**: one dedicated SSH connection (own TCP stream)
/// per writer. OpenSSH does not set `TCP_NODELAY` on non-interactive
/// sessions, so when several SFTP channels share ONE TCP stream the
/// server's small replies queue behind each other's delayed ACKs (Nagle);
/// separate streams keep each writer in plain request/reply ping-pong and
/// also lift the per-connection `MaxSessions` channel cap. `channel` keeps
/// every writer on the shared connection
/// (`RBACKUP_SFTP_WRITERS=channel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriterTransport {
    Connection,
    Channel,
}

impl WriterTransport {
    fn from_env() -> Self {
        match std::env::var("RBACKUP_SFTP_WRITERS") {
            Ok(v) if v.eq_ignore_ascii_case("channel") => Self::Channel,
            _ => Self::Connection,
        }
    }
}

/// Environment override: `RBACKUP_SFTP_DURABILITY=fsync` forces the
/// per-chunk fsync path even when the server allows the batched barrier.
const DURABILITY_ENV: &str = "RBACKUP_SFTP_DURABILITY";
/// Deferred chunks per writer before a barrier + rename batch.
const BARRIER_BATCH_CHUNKS: usize = 256;
/// Deferred bytes per writer before a barrier + rename batch.
const BARRIER_BATCH_BYTES: usize = 128 * 1024 * 1024;
/// First line printed by the remote barrier loop once it runs.
const BARRIER_READY: &str = "rbackup-barrier-ready";
/// Remote barrier loop: one `sync` per request line, answered `ok`/`fail`.
/// Plain POSIX shell, run through the login shell of the SSH user.
const BARRIER_COMMAND: &str = "echo rbackup-barrier-ready; while read -r _; do if sync; then echo ok; else echo fail; fi; done";
const BARRIER_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// A global `sync` may have to flush a lot of dirty data on a slow disk.
const BARRIER_SYNC_TIMEOUT: Duration = Duration::from_secs(600);

/// Server-wide durability barrier: a long-lived exec channel running a
/// tiny `sync` loop. One `sync(2)` flushes every file the writers closed,
/// which replaces one server fsync per chunk (the dominant cost on slow
/// filesystems such as NTFS through ntfs-3g FUSE).
///
/// Unavailable when the account cannot run commands (for example a key
/// restricted with `command="internal-sftp"`): the probe then fails and the
/// writers keep the per-chunk fsync path.
struct RemoteBarrier {
    channel: tokio::sync::Mutex<BarrierChannel>,
}

struct BarrierChannel {
    channel: russh::Channel<russh::client::Msg>,
    buf: Vec<u8>,
}

impl BarrierChannel {
    /// Reads one newline-terminated line of the remote loop's stdout.
    async fn read_line(&mut self) -> crate::Result<String> {
        loop {
            if let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=pos).collect();
                return Ok(String::from_utf8_lossy(&line).trim().to_string());
            }
            match self.channel.wait().await {
                Some(russh::ChannelMsg::Data { data }) => self.buf.extend_from_slice(&data),
                Some(
                    russh::ChannelMsg::Failure
                    | russh::ChannelMsg::Eof
                    | russh::ChannelMsg::Close
                    | russh::ChannelMsg::ExitStatus { .. }
                    | russh::ChannelMsg::ExitSignal { .. },
                )
                | None => return Err(barrier_error("remote sync channel closed")),
                Some(_) => {}
            }
        }
    }
}

impl RemoteBarrier {
    /// Starts the remote loop and waits for its ready line; `None` when the
    /// server refuses or never answers (the channel is closed again).
    async fn open(session: &russh::client::Handle<ClientHandler>) -> Option<Self> {
        let channel = tokio::time::timeout(BARRIER_PROBE_TIMEOUT, session.channel_open_session())
            .await
            .ok()?
            .ok()?;
        let mut ch = BarrierChannel {
            channel,
            buf: Vec::new(),
        };
        let ready = tokio::time::timeout(BARRIER_PROBE_TIMEOUT, async {
            ch.channel.exec(true, BARRIER_COMMAND).await.ok()?;
            let line = ch.read_line().await.ok()?;
            Some(line == BARRIER_READY)
        })
        .await;
        if matches!(ready, Ok(Some(true))) {
            Some(Self {
                channel: tokio::sync::Mutex::new(ch),
            })
        } else {
            let _ = ch.channel.close().await;
            None
        }
    }

    /// Runs one remote `sync` and waits for it to complete.
    async fn sync(&self) -> crate::Result<()> {
        let mut ch = self.channel.lock().await;
        tokio::time::timeout(BARRIER_SYNC_TIMEOUT, async {
            ch.channel
                .data(&b"s\n"[..])
                .await
                .map_err(|e| barrier_error(&format!("remote sync request: {e}")))?;
            match ch.read_line().await?.as_str() {
                "ok" => Ok(()),
                other => Err(barrier_error(&format!("remote sync failed ({other})"))),
            }
        })
        .await
        .map_err(|_| barrier_error("remote sync timed out"))?
    }
}

fn barrier_error(msg: &str) -> crate::Error {
    crate::Error::Io(std::io::Error::other(msg.to_string()))
}

/// Parallel chunk writer over a dedicated SFTP channel.
///
/// Skips what the general-purpose [`StorageBackend::write_atomic`] must do
/// per call: no existence probe (the pipeline's in-memory index plus the
/// exclusive repository lock guarantee the target is absent) and no
/// per-segment directory probing after the first write into a prefix
/// directory.
///
/// Durability follows the [`ChunkWriter`] contract in one of two modes:
/// - **batched sync** (server allows the [`RemoteBarrier`]): chunks are
///   written and closed under a temp name WITHOUT fsync; every
///   [`BARRIER_BATCH_CHUNKS`] chunks / [`BARRIER_BATCH_BYTES`] bytes one
///   remote `sync` makes the batch durable, then the batch is renamed into
///   place; `finish` flushes the last batch and syncs once more so the
///   renames are durable before the manifest is written. A crash leaves
///   only temp files (ignored by the chunk index, swept by prune).
/// - **per-chunk fsync** (fallback): temp name, `fsync@openssh.com`, rename.
struct SftpChunkWriter {
    runtime: Arc<tokio::runtime::Runtime>,
    sftp: Arc<SftpSession>,
    /// Dedicated SSH connection of this writer (kept alive while it
    /// writes); `None` when the writer is a channel on the shared session.
    _connection: Option<russh::client::Handle<ClientHandler>>,
    remote_root: String,
    dirs: HashSet<String>,
    barrier: Option<Arc<RemoteBarrier>>,
    /// Closed-but-unsynced chunks of the current batch: `(temp, final)`.
    pending: Vec<(String, String)>,
    pending_bytes: usize,
    /// Renames done since the last barrier (batched mode).
    renamed_since_sync: bool,
    /// Per-operation timings, reported with `RBACKUP_PROFILE`.
    op_times: OpTimes,
}

/// Cumulative per-operation timings of one SFTP chunk writer (seconds).
#[derive(Default)]
struct OpTimes {
    chunks: u64,
    mkdir: f64,
    open: f64,
    write: f64,
    close: f64,
    batch: f64,
}

impl SftpChunkWriter {
    /// Batched mode: one remote `sync`, then rename the whole batch.
    fn flush_batch(&mut self) -> crate::Result<()> {
        let Some(barrier) = self.barrier.clone() else {
            return Ok(());
        };
        if self.pending.is_empty() {
            return Ok(());
        }
        let batch = std::mem::take(&mut self.pending);
        self.pending_bytes = 0;
        let sftp = Arc::clone(&self.sftp);
        let started = std::time::Instant::now();
        let flushed = self.runtime.block_on(async move {
            barrier.sync().await?;
            for (tmp, path) in &batch {
                // SFTP v3 rename refuses an existing target: a target that
                // already exists despite the index is a bug worth failing on.
                sftp.rename(tmp, path).await?;
            }
            Ok::<(), crate::Error>(())
        });
        self.op_times.batch += started.elapsed().as_secs_f64();
        flushed?;
        self.renamed_since_sync = true;
        Ok(())
    }
}

impl ChunkWriter for SftpChunkWriter {
    fn write_new(&mut self, rel: &str, data: &[u8]) -> crate::Result<()> {
        let path = format!(
            "{}/{}",
            self.remote_root.trim_end_matches('/'),
            rel.trim_start_matches('/')
        );
        let tmp = format!("{path}.tmp-{}", uuid::Uuid::new_v4().simple());
        let parent = parent_of(&path);
        let need_mkdirs = parent.as_ref().is_some_and(|p| !self.dirs.contains(p));

        let sftp = Arc::clone(&self.sftp);
        let mkdir_parent = parent.clone();
        let batched = self.barrier.is_some();
        let times = &mut self.op_times;
        let result = self.runtime.block_on(async {
            let t = std::time::Instant::now();
            if need_mkdirs && let Some(parent) = &mkdir_parent {
                mkdirs_remote(&sftp, parent).await?;
            }
            times.mkdir += t.elapsed().as_secs_f64();
            let written: crate::Result<()> = async {
                let t = std::time::Instant::now();
                let mut file = sftp
                    .open_with_flags(
                        &tmp,
                        russh_sftp::protocol::OpenFlags::CREATE
                            | russh_sftp::protocol::OpenFlags::WRITE
                            | russh_sftp::protocol::OpenFlags::TRUNCATE,
                    )
                    .await?;
                times.open += t.elapsed().as_secs_f64();
                let t = std::time::Instant::now();
                file.write_all(data).await?;
                times.write += t.elapsed().as_secs_f64();
                let t = std::time::Instant::now();
                if !batched {
                    file.sync_all().await?;
                }
                file.shutdown().await?;
                times.close += t.elapsed().as_secs_f64();
                if !batched {
                    // SFTP v3 rename refuses an existing target, which is
                    // exactly the wanted behavior here: a target that
                    // already exists despite the index would be a bug.
                    sftp.rename(&tmp, &path).await?;
                }
                Ok(())
            }
            .await;
            if written.is_err() {
                let _ = sftp.remove_file(&tmp).await;
            }
            written
        });
        // Cache the parent only after full success so a failed first write
        // retries the directory check next time.
        if result.is_ok()
            && let Some(parent) = parent
        {
            self.dirs.insert(parent);
        }
        result?;
        self.op_times.chunks += 1;
        if batched {
            self.pending.push((tmp, path));
            self.pending_bytes += data.len();
            if self.pending.len() >= BARRIER_BATCH_CHUNKS
                || self.pending_bytes >= BARRIER_BATCH_BYTES
            {
                self.flush_batch()?;
            }
        }
        Ok(())
    }

    fn finish(&mut self) -> crate::Result<()> {
        self.flush_batch()?;
        if let Some(barrier) = self.barrier.clone()
            && self.renamed_since_sync
        {
            // Make the renames themselves durable before any manifest
            // refers to these chunks.
            self.runtime.block_on(async move { barrier.sync().await })?;
            self.renamed_since_sync = false;
        }
        if std::env::var_os("RBACKUP_PROFILE").is_some() {
            let t = &self.op_times;
            eprintln!(
                "sftp writer: {} chunks | mkdir {:.2}s | open {:.2}s | write {:.2}s | close {:.2}s | \
                 batches {:.2}s",
                t.chunks, t.mkdir, t.open, t.write, t.close, t.batch
            );
        }
        Ok(())
    }
}

async fn connect_inner(
    params: &SftpConnect,
) -> crate::Result<(SftpSession, russh::client::Handle<ClientHandler>)> {
    // The engine issues many small SFTP requests (open/write/fsync/rename);
    // Nagle's algorithm would serialize each request/ack exchange behind
    // delayed ACKs on every chunk.
    let config = Arc::new(russh::client::Config {
        nodelay: true,
        ..russh::client::Config::default()
    });
    let handler = ClientHandler {
        policy: params.host_key_policy.clone(),
        host_tag: format!("{}:{}", params.host, params.port),
    };
    let mut session = russh::client::connect(config, (params.host.as_str(), params.port), handler)
        .await
        .map_err(|e| crate::Error::Io(std::io::Error::other(format!("ssh connect: {e}"))))?;

    let key = russh::keys::load_secret_key(&params.key_path, params.key_passphrase.as_deref())
        .map_err(|e| crate::Error::Repo(format!("cannot load private key: {e}")))?;
    let auth = session
        .authenticate_publickey(
            &params.username,
            russh::keys::PrivateKeyWithHashAlg::new(
                Arc::new(key),
                Some(russh::keys::HashAlg::Sha256),
            ),
        )
        .await
        .map_err(|e| crate::Error::Repo(format!("public key auth failed: {e}")))?;
    if !auth.success() {
        return Err(crate::Error::Repo(
            "public key authentication refused by server".into(),
        ));
    }

    let sftp = open_sftp_channel(&session).await?;
    mkdirs_remote(&sftp, &params.remote_root).await?;
    Ok((sftp, session))
}

/// Opens one more SFTP session on the live SSH connection.
async fn open_sftp_channel(
    session: &russh::client::Handle<ClientHandler>,
) -> crate::Result<SftpSession> {
    let channel = session
        .channel_open_session()
        .await
        .map_err(|e| crate::Error::Io(std::io::Error::other(format!("channel: {e}"))))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| crate::Error::Io(std::io::Error::other(format!("subsystem: {e}"))))?;
    SftpSession::new_with_config(channel.into_stream(), sftp_client_config())
        .await
        .map_err(|e| crate::Error::Repo(format!("sftp init: {e}")))
}

/// Tuned russh-sftp client config for bulk chunk traffic: large framed
/// writes (fewer requests per chunk), deeper write pipelining and a
/// timeout generous enough for a slow server-side fsync.
fn sftp_client_config() -> russh_sftp::client::Config {
    russh_sftp::client::Config {
        max_packet_len: 262_144,
        max_concurrent_reads: 16,
        max_concurrent_writes: 32,
        max_write_packet_len: 131_072,
        request_timeout_secs: 60,
    }
}

struct ClientHandler {
    policy: HostKeyPolicy,
    host_tag: String,
}

impl russh::client::Handler for ClientHandler {
    type Error = crate::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        match &self.policy {
            HostKeyPolicy::AcceptAll => Ok(true),
            HostKeyPolicy::TrustOnFirstUse { known_hosts } => {
                let russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } = server_public_key
                else {
                    // SSH certificates are not supported for host keys.
                    return Ok(false);
                };
                let fingerprint = format!("{}", key.fingerprint(russh::keys::HashAlg::Sha256));
                let host_line = format!("{} {}", self.host_tag, fingerprint);
                if known_hosts.exists() {
                    let stored = std::fs::read_to_string(known_hosts)?;
                    if stored.lines().any(|l| l == host_line) {
                        return Ok(true);
                    }
                    return Err(crate::Error::Repo(
                        "host key changed or unknown (known_hosts mismatch) — refusing to connect"
                            .into(),
                    ));
                }
                if let Some(parent) = known_hosts.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                use std::io::Write as _;
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(known_hosts)?;
                writeln!(file, "{host_line}")?;
                Ok(true)
            }
        }
    }
}

fn parent_of(path: &str) -> Option<String> {
    path.rsplit_once('/').map(|(parent, _)| parent.to_string())
}

/// Creates `path` and every missing parent on the remote side.
pub(crate) async fn mkdirs_remote(sftp: &SftpSession, path: &str) -> crate::Result<()> {
    let clean = path.trim_matches('/');
    let mut current = String::new();
    for segment in clean.split('/') {
        if segment.is_empty() {
            continue;
        }
        current.push('/');
        current.push_str(segment);
        if sftp.metadata(&current).await.is_ok() {
            continue;
        }
        if let Err(e) = sftp.create_dir(&current).await {
            // Lost a race or it already exists: good enough.
            if sftp.metadata(&current).await.is_err() {
                return Err(sftp_error(format!("mkdir {current}: {e}")));
            }
        }
    }
    Ok(())
}

/// Depth-first recursive listing returning repo-relative paths.
async fn walk_remote(
    sftp: &SftpSession,
    remote_dir: &str,
    rel_prefix: &str,
) -> crate::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut stack = vec![(remote_dir.to_string(), rel_prefix.to_string())];
    while let Some((dir, rel_dir)) = stack.pop() {
        let entries = sftp
            .read_dir(&dir)
            .await
            .map_err(|e| sftp_error(format!("read_dir {}: {e}", dir)))?;
        for entry in entries {
            let name = entry.file_name();
            if name == "." || name == ".." {
                continue;
            }
            let rel = if rel_prefix.is_empty() {
                name.clone()
            } else {
                format!("{rel_dir}/{name}")
            };
            let is_dir = entry.metadata().is_dir();
            if is_dir {
                stack.push((format!("{dir}/{name}"), rel.clone()));
            } else {
                out.push(rel);
            }
        }
    }
    Ok(out)
}

fn sftp_error(message: String) -> crate::Error {
    crate::Error::Repo(message)
}
