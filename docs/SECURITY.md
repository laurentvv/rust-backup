# Security design

## Threat model

Protects the confidentiality and integrity of backed-up data against: theft/loss of the backup medium (USB disk, server compromise of the backup account), silent bit rot, and accidental or malicious modification of stored data. Not in scope: a compromised machine at backup time (the plaintext and passphrase are in memory there), keyloggers, and side channels on the user's own host.

## Cryptography (encrypt-then-store, per chunk)

- **Master key**: 32 random bytes per repository, zeroized on drop, never written in the clear, never printed (Debug redacted).
- **Key wrap**: Argon2id (RFC 9106 low-memory profile: m=64 MiB, t=3, p=4; parameters stored in the key file so they can be raised later) derives a KEK from the passphrase; the master key is sealed with AES-256-GCM, AD `rustbackup/masterkey/v1`.
- **Chunks**: plaintext → zstd (default level 5) → AES-256-GCM with a fresh 96-bit random nonce; **associated data = BLAKE3(plaintext) ‖ plain_len**. Swapping chunks between files, truncating, or tampering with ciphertext fails the MAC on restore/verify (tests cover each case).
- **Manifests**: sealed with the same key, AD = `"RBSN" ‖ snapshot id ‖ plain_len`.

Compression happens **before** encryption (compressing ciphertext is useless; this is the restic/borg/kopia order).

## Host keys (SFTP)

Trust-on-first-use: the SHA-256 fingerprint of the server key is pinned per `host:port` in a local known-hosts file; a later mismatch refuses the connection (interception guard). `AcceptAll` exists for the automated test suite only and is never exposed in the GUI.

## Secret handling

- Repository passphrase: `RBACKUP_PASSPHRASE` env (CLI) or an in-memory session field (GUI) — never a CLI argument, never logged, never echoed. Tests assert the error path leaks no passphrase.
- SSH key passphrase: `RBACKUP_KEY_PASSPHRASE` (CLI), passed through to key decryption only.
- Encrypted SSH keys (e.g. AES-256-CTR OpenSSH format) are supported.

## Source integrity

The engine opens sources read-only; no code path writes to, or deletes from, a source. Restores write to temp names and rename; the overwrite policy decides whether an existing target is replaced. Destructive pruning (future) will only ever target the backup destination behind an explicit confirmation.

## Verification

`verify --full` reads, authenticates and re-hashes every referenced chunk (MAC failure or hash mismatch names the offending chunk); `verify --quick` checks manifest authentication and chunk existence. Orphan chunks are reported as information (space, not integrity).
