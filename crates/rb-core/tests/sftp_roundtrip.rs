//! End-to-end SFTP tests against an in-process SSH/SFTP server backed by a
//! real temporary directory — the full public-key auth + repository path.

use std::collections::HashMap;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use russh::keys::ssh_key;
use russh::server::Server as _;
use russh::server::{Auth, ChannelOpenHandle, Msg, Session, VerifiedPublicKey};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Packet, Status, StatusCode, Version,
};
use uuid::Uuid;

use rb_core::backend::StorageBackend;
use rb_core::backend::sftp::{HostKeyPolicy, SftpBackend, SftpConnect};
use rb_core::backup::{BackupOptions, run_backup};
use rb_core::chunker::ChunkerConfig;
use rb_core::crypto::KdfParams;
use rb_core::domain::{Destination, JobConfig};
use rb_core::repo::Repository;
use rb_core::restore::{RestoreDestination, RestoreOptions, RestoreSelection, restore};
use rb_core::verify::{VerifyMode, verify};

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

// ---------------------------------------------------------------------------
// In-process SSH + SFTP test server
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct TestSshServer {
    expected_public: ssh_key::PublicKey,
    fs_root: PathBuf,
}

impl russh::server::Server for TestSshServer {
    type Handler = TestSshSession;

    fn new_client(&mut self, _addr: Option<std::net::SocketAddr>) -> Self::Handler {
        TestSshSession {
            expected_public: self.expected_public.clone(),
            fs_root: self.fs_root.clone(),
            channels: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }
}

struct TestSshSession {
    expected_public: ssh_key::PublicKey,
    fs_root: PathBuf,
    channels: Arc<tokio::sync::Mutex<HashMap<ChannelId, Channel<Msg>>>>,
}

impl russh::server::Handler for TestSshSession {
    type Error = russh::Error;

    async fn auth_publickey(
        &mut self,
        _user: &str,
        key: VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        if *key.public_key == self.expected_public {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.lock().await.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name == "sftp" {
            let channel = self.channels.lock().await.remove(&channel_id);
            if let Some(channel) = channel {
                session.channel_success(channel_id)?;
                let handler = FsSftp::new(self.fs_root.clone());
                russh_sftp::server::run(channel.into_stream(), handler).await;
            } else {
                session.channel_failure(channel_id)?;
            }
        } else {
            session.channel_failure(channel_id)?;
        }
        Ok(())
    }
}

/// Filesystem-backed SFTP handler; "/" maps to `root`.
struct FsSftp {
    root: PathBuf,
    handles: HashMap<String, HandleState>,
    next: u64,
}

enum HandleState {
    Read(std::fs::File),
    Write(std::fs::File),
    Dir(Vec<(String, bool)>, usize),
}

impl FsSftp {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            handles: HashMap::new(),
            next: 0,
        }
    }

    fn resolve(&self, sftp_path: &str) -> Result<PathBuf, StatusCode> {
        let mut out = self.root.clone();
        for segment in sftp_path.split('/') {
            if segment.is_empty() || segment == "." {
                continue;
            }
            if segment == ".." {
                return Err(StatusCode::PermissionDenied);
            }
            out.push(segment);
        }
        Ok(out)
    }
}

fn ok_status(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".to_string(),
        language_tag: "en-US".to_string(),
    }
}

fn attrs_for(path: &Path) -> FileAttributes {
    let mut attrs = FileAttributes::default();
    if path.is_dir() {
        attrs.set_dir(true);
    } else {
        attrs.set_regular(true);
    }
    attrs
}

fn io_status(e: &std::io::Error) -> StatusCode {
    if e.kind() == std::io::ErrorKind::NotFound {
        StatusCode::NoSuchFile
    } else {
        StatusCode::Failure
    }
}

impl russh_sftp::server::Handler for FsSftp {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        version: u32,
        _extensions: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        let _ = version;
        Ok(Version::new())
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let path = self.resolve(&filename)?;
        let mut opts = std::fs::OpenOptions::new();
        opts.read(pflags.contains(OpenFlags::READ));
        if pflags.contains(OpenFlags::WRITE) {
            opts.write(true);
        }
        if pflags.contains(OpenFlags::APPEND) {
            opts.append(true);
        }
        if pflags.contains(OpenFlags::CREATE) {
            opts.create(true);
        }
        if pflags.contains(OpenFlags::TRUNCATE) {
            opts.truncate(true);
        }
        let file = opts.open(&path).map_err(|e| io_status(&e))?;
        self.next += 1;
        let handle = format!("h{}", self.next);
        let state = if pflags.contains(OpenFlags::WRITE) || pflags.contains(OpenFlags::APPEND) {
            HandleState::Write(file)
        } else {
            HandleState::Read(file)
        };
        self.handles.insert(handle.clone(), state);
        Ok(Handle { id, handle })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.handles.remove(&handle);
        Ok(ok_status(id))
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        let state = self.handles.get_mut(&handle).ok_or(StatusCode::Failure)?;
        let HandleState::Read(file) = state else {
            return Err(StatusCode::Failure);
        };
        use std::io::{Read as _, Seek, SeekFrom};
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| io_status(&e))?;
        let mut buf = vec![0u8; len as usize];
        let n = file.read(&mut buf).map_err(|e| io_status(&e))?;
        if n == 0 {
            return Err(StatusCode::Eof);
        }
        buf.truncate(n);
        Ok(Data { id, data: buf })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        let state = self.handles.get_mut(&handle).ok_or(StatusCode::Failure)?;
        let HandleState::Write(file) = state else {
            return Err(StatusCode::Failure);
        };
        use std::io::{Seek, SeekFrom, Write as _};
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| io_status(&e))?;
        file.write_all(&data).map_err(|e| io_status(&e))?;
        Ok(ok_status(id))
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let dir = self.resolve(&path)?;
        let mut names: Vec<(String, bool)> = Vec::new();
        for entry in std::fs::read_dir(&dir).map_err(|e| io_status(&e))? {
            let entry = entry.map_err(|e| io_status(&e))?;
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            names.push((entry.file_name().to_string_lossy().to_string(), is_dir));
        }
        names.sort();
        self.next += 1;
        let handle = format!("d{}", self.next);
        self.handles
            .insert(handle.clone(), HandleState::Dir(names, 0));
        Ok(Handle { id, handle })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        let state = self.handles.get_mut(&handle).ok_or(StatusCode::Failure)?;
        let HandleState::Dir(entries, index) = state else {
            return Err(StatusCode::Failure);
        };
        if *index >= entries.len() {
            return Err(StatusCode::Eof);
        }
        let mut files = Vec::new();
        // "." and ".." for protocol compatibility.
        if *index == 0 {
            files.push(File::new(".", FileAttributes::default()));
            files.push(File::new("..", FileAttributes::default()));
        }
        let batch = 64;
        while *index < entries.len() && files.len() < batch {
            let (name, is_dir) = entries[*index].clone();
            *index += 1;
            let mut attrs = FileAttributes::default();
            if is_dir {
                attrs.set_dir(true);
            } else {
                attrs.set_regular(true);
            }
            files.push(File::new(&name, attrs));
        }
        Ok(Name { id, files })
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let dir = self.resolve(&path)?;
        std::fs::create_dir(&dir).map_err(|e| io_status(&e))?;
        Ok(ok_status(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        let dir = self.resolve(&path)?;
        std::fs::remove_dir(&dir).map_err(|e| io_status(&e))?;
        Ok(ok_status(id))
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        let path = self.resolve(&filename)?;
        std::fs::remove_file(&path).map_err(|e| io_status(&e))?;
        Ok(ok_status(id))
    }

    async fn rename(&mut self, id: u32, old: String, new: String) -> Result<Status, Self::Error> {
        let from = self.resolve(&old)?;
        let to = self.resolve(&new)?;
        if to.exists() {
            std::fs::remove_file(&to).map_err(|e| io_status(&e))?;
        }
        std::fs::rename(&from, &to).map_err(|e| io_status(&e))?;
        Ok(ok_status(id))
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let target = self.resolve(&path)?;
        if !target.exists() {
            return Err(StatusCode::NoSuchFile);
        }
        let attrs = attrs_for(&target);
        Ok(Attrs { id, attrs })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        self.stat(id, path).await
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        let _ = &self.handles[&handle];
        Ok(Attrs {
            id,
            attrs: FileAttributes::default(),
        })
    }

    async fn setstat(
        &mut self,
        id: u32,
        _path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        Ok(ok_status(id))
    }

    async fn realpath(&mut self, id: u32, _path: String) -> Result<Name, Self::Error> {
        Ok(Name {
            id,
            files: vec![File::new("/", FileAttributes::default())],
        })
    }

    async fn extended(
        &mut self,
        _id: u32,
        _request: String,
        _data: Vec<u8>,
    ) -> Result<Packet, Self::Error> {
        Err(StatusCode::OpUnsupported)
    }
}

/// Spawns the test server on a free localhost port; returns (port, root).
fn spawn_server(expected_public: ssh_key::PublicKey) -> (u16, PathBuf) {
    let root = std::env::temp_dir().join(format!("rb-sftp-root-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("root dir");

    let listener = TcpListener::bind("127.0.0.1:0").expect("probe bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);

    let server = TestSshServer {
        expected_public,
        fs_root: root.clone(),
    };
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("server runtime");
        runtime.block_on(async move {
            let config = russh::server::Config {
                keys: vec![
                    russh::keys::PrivateKey::random(&mut KeyRng, russh::keys::Algorithm::Ed25519)
                        .expect("host key"),
                ],
                ..Default::default()
            };
            let mut server = server;
            server
                .run_on_address(Arc::new(config), ("127.0.0.1", port))
                .await
                .expect("server run");
        });
    });
    (port, root)
}

/// Adapter exposing rand 0.9 randomness as ssh-key's rand_core RNG.
struct KeyRng;

impl ssh_key::rand_core::TryRng for KeyRng {
    type Error = std::convert::Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        Ok(rand::random())
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        Ok(rand::random())
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Self::Error> {
        use rand::RngCore as _;
        rand::rng().fill_bytes(dest);
        Ok(())
    }
}

impl ssh_key::rand_core::TryCryptoRng for KeyRng {}

/// Generates an ed25519 keypair; returns (private PEM file, public key).
fn write_key_file(passphrase: Option<&str>) -> (PathBuf, ssh_key::PublicKey) {
    let mut rng = KeyRng;
    let key = russh::keys::PrivateKey::random(&mut rng, russh::keys::Algorithm::Ed25519)
        .expect("generate key");
    let final_key = match passphrase {
        Some(pass) => key.encrypt(&mut rng, pass).expect("encrypt"),
        None => key,
    };
    let pem = final_key
        .to_openssh(ssh_key::LineEnding::LF)
        .expect("pem")
        .to_string();
    let path = std::env::temp_dir().join(format!("rb-sftp-key-{}.key", Uuid::new_v4()));
    std::fs::write(&path, pem).expect("write key");
    (path, final_key.public_key().clone())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

fn connect_params(port: u16, key_path: &Path, passphrase: Option<String>) -> SftpConnect {
    SftpConnect {
        host: "127.0.0.1".into(),
        port,
        username: "tester".into(),
        key_path: key_path.to_path_buf(),
        key_passphrase: passphrase,
        remote_root: format!("/repo-{}", Uuid::new_v4().simple()),
        host_key_policy: HostKeyPolicy::AcceptAll,
    }
}

#[test]
fn sftp_backup_verify_restore_roundtrip() {
    let (key_path, public) = write_key_file(None);
    let (port, server_root) = spawn_server(public);

    let base = std::env::temp_dir().join(format!("rb-sftp-it-{}", Uuid::new_v4()));
    let source = base.join("src");
    std::fs::create_dir_all(source.join("docs")).expect("dirs");
    std::fs::write(source.join("data.bin"), random_bytes(1_200_000, 3)).expect("data");
    std::fs::write(source.join("docs/notes.txt"), b"sftp notes\n".repeat(64)).expect("notes");

    let backend = SftpBackend::connect(&connect_params(port, &key_path, None)).expect("connect");
    assert_eq!(backend.kind(), "sftp");

    let repo = Repository::init(
        Arc::new(backend.clone()),
        "sftp passphrase",
        ChunkerConfig::default(),
        5,
        fast_kdf(),
    )
    .expect("init over sftp");

    let job = JobConfig {
        id: Uuid::new_v4(),
        name: "sftp-job".into(),
        sources: vec![source.clone()],
        destination: Destination::Sftp {
            host: "127.0.0.1".into(),
            port,
            username: "tester".into(),
            key_path: key_path.clone(),
            remote_root: backend
                .describe()
                .rsplit(':')
                .next()
                .unwrap_or_default()
                .to_string(),
        },
        compression_level: 5,
        excludes: vec![],
        max_incrementals: 10,
        created_at: chrono::Utc::now(),
    };

    let (manifest, stats) =
        run_backup(&repo, &job, &BackupOptions::default(), |_| Ok(())).expect("backup over sftp");
    assert!(stats.chunks_new >= 1);
    assert_eq!(manifest.file_count(), 2);

    // Chunks physically live under the server's temp root.
    let repo_dir = server_root.join(
        backend
            .describe()
            .rsplit(':')
            .next()
            .unwrap_or_default()
            .trim_start_matches('/'),
    );
    let chunks_dir = repo_dir.join("chunks");
    let chunk_count = walkdir::WalkDir::new(&chunks_dir)
        .min_depth(1)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .count();
    assert_eq!(chunk_count as u64, stats.chunks_new, "chunks on server");

    // Second run: zero new chunks (dedup over SFTP).
    let (_m2, s2) =
        run_backup(&repo, &job, &BackupOptions::default(), |_| Ok(())).expect("second backup");
    assert_eq!(s2.chunks_new, 0);

    // Verify full.
    let report = verify(&repo, VerifyMode::Full, None, |_, _| Ok(())).expect("verify");
    assert!(report.ok(), "issues: {:?}", report.issues);

    // Restore one file to a chosen destination and compare bytes.
    let target = base.join("restored");
    let stats = restore(
        &repo,
        &manifest.id,
        &[RestoreSelection {
            source_root: source.clone(),
            rel_path: PathBuf::from("docs/notes.txt"),
        }],
        &RestoreDestination::Target(target.clone()),
        &RestoreOptions::default(),
        |_| Ok(()),
    )
    .expect("restore over sftp");
    assert_eq!(stats.files_restored, 1);
    assert_eq!(
        std::fs::read(target.join("src/docs/notes.txt")).expect("restored"),
        std::fs::read(source.join("docs/notes.txt")).expect("original")
    );

    backend.close().ok();
}

#[test]
fn sftp_wrong_public_key_is_rejected() {
    let (_good_key, good_public) = write_key_file(None);
    let (port, _root) = spawn_server(good_public);
    let (bad_key, _bad_public) = write_key_file(None);

    let err = SftpBackend::connect(&connect_params(port, &bad_key, None)).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("refused") || msg.contains("auth"),
        "auth rejection expected, got: {msg}"
    );
}

#[test]
fn sftp_passphrase_protected_key_is_supported() {
    let (key_path, public) = write_key_file(Some("key-secret-passphrase"));
    let (port, _root) = spawn_server(public);

    // Without the passphrase: load fails cleanly.
    let err = SftpBackend::connect(&connect_params(port, &key_path, None)).unwrap_err();
    assert!(
        err.to_string().contains("private key"),
        "expected key-load error, got: {err}"
    );

    // With the passphrase: full connect.
    let backend = SftpBackend::connect(&connect_params(
        port,
        &key_path,
        Some("key-secret-passphrase".into()),
    ))
    .expect("connect with passphrase key");
    backend.close().ok();
}

#[test]
fn sftp_host_key_tofu_pins_and_detects_changes() {
    let (key_path, public) = write_key_file(None);
    let (port, _root) = spawn_server(public);

    let known_hosts = std::env::temp_dir().join(format!("rb-known-{}", Uuid::new_v4()));
    let params = SftpConnect {
        host: "127.0.0.1".into(),
        port,
        username: "tester".into(),
        key_path,
        key_passphrase: None,
        remote_root: format!("/tofu-{}", Uuid::new_v4().simple()),
        host_key_policy: HostKeyPolicy::TrustOnFirstUse {
            known_hosts: known_hosts.clone(),
        },
    };

    let first = SftpBackend::connect(&params).expect("first connect (TOFU)");
    assert!(known_hosts.exists(), "known_hosts written on first use");
    first.close().ok();

    let second = SftpBackend::connect(&params).expect("second connect (pinned)");
    second.close().ok();

    // Tampering with the pinned fingerprint must refuse the connection.
    let stored = std::fs::read_to_string(&known_hosts).expect("read known_hosts");
    let tampered = stored.replace("SHA256", "SHA256:TAMPERED");
    std::fs::write(&known_hosts, tampered).expect("write known_hosts");
    let err = SftpBackend::connect(&params).unwrap_err();
    assert!(err.to_string().contains("known_hosts"), "got: {err}");
}
