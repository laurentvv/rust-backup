//! SFTP backend: repository on a remote Linux server, public-key auth.

use std::path::PathBuf;
use std::sync::Arc;

use russh_sftp::client::SftpSession;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::StorageBackend;

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
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| crate::Error::Io(std::io::Error::other(e.to_string())))?;

        let sftp = runtime.block_on(connect_inner(params))?;
        Ok(Self {
            runtime: Arc::new(runtime),
            sftp: Arc::new(sftp),
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
            // Write the full content under a temp name...
            let mut file = sftp
                .open_with_flags(
                    &tmp,
                    russh_sftp::protocol::OpenFlags::CREATE
                        | russh_sftp::protocol::OpenFlags::WRITE
                        | russh_sftp::protocol::OpenFlags::TRUNCATE,
                )
                .await?;
            file.write_all(&data).await?;
            file.shutdown().await?;

            // Rename into place (atomic); replace any previous version.
            if sftp.metadata(&path).await.is_ok() {
                sftp.remove_file(&path).await?;
            }
            sftp.rename(&tmp, &path).await?;
            Ok(())
        })
    }

    fn exists(&self, rel: &str) -> crate::Result<bool> {
        let path = self.remote_path(rel);
        let sftp = Arc::clone(&self.sftp);
        self.runtime
            .block_on(async move { Ok(sftp.metadata(&path).await.is_ok()) })
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
}

async fn connect_inner(params: &SftpConnect) -> crate::Result<SftpSession> {
    let config = Arc::new(russh::client::Config::default());
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

    let channel = session
        .channel_open_session()
        .await
        .map_err(|e| crate::Error::Io(std::io::Error::other(format!("channel: {e}"))))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| crate::Error::Io(std::io::Error::other(format!("subsystem: {e}"))))?;
    let sftp = SftpSession::new(channel.into_stream())
        .await
        .map_err(|e| crate::Error::Repo(format!("sftp init: {e}")))?;

    mkdirs_remote(&sftp, &params.remote_root).await?;
    Ok(sftp)
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
