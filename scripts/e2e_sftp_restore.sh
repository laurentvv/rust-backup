#!/usr/bin/env bash
# End-to-end check against a real SFTP server: full backup, single-file restore,
# directory restore, byte-for-byte comparison, full verify. Throwaway passphrase
# and repository (deleted at the end).
#
# Required environment: E2E_HOST, E2E_USER, E2E_KEY, E2E_ROOT (scratch dir on the server),
# E2E_SOURCE (local folder to back up), E2E_FILE (a file path relative to E2E_SOURCE),
# E2E_DIR (a directory path relative to E2E_SOURCE). Optional: RBACKUP.
set -euo pipefail
export MSYS_NO_PATHCONV=1   # keep /remote/paths intact (AGENTS.md lesson 2026-11-08)

: "${E2E_HOST:?}" "${E2E_USER:?}" "${E2E_KEY:?}" "${E2E_ROOT:?}" "${E2E_SOURCE:?}" "${E2E_FILE:?}" "${E2E_DIR:?}"
RBACKUP="${RBACKUP:-target/release/rbackup.exe}"
export RBACKUP_PASSPHRASE="e2e-only-throwaway"
SSH=(ssh -i "$E2E_KEY" -o IdentitiesOnly=yes -o BatchMode=yes "$E2E_USER@$E2E_HOST")
WORK="$(cygpath -m "$(mktemp -d)")"   # Windows path: the exe must not see an MSYS /tmp path
trap 'rm -rf "$WORK"; "${SSH[@]}" "rm -rf '"'$E2E_ROOT'"'"' EXIT

db=(--db "$WORK/e2e.duckdb")
sftp=(--sftp-host "$E2E_HOST" --sftp-user "$E2E_USER" --sftp-key "$E2E_KEY" --sftp-root "$E2E_ROOT")
src_name="$(basename "$E2E_SOURCE")"

"${SSH[@]}" "rm -rf '$E2E_ROOT'"
"$RBACKUP" "${db[@]}" repo init "${sftp[@]}"
"$RBACKUP" "${db[@]}" jobs add --name e2e --source "$E2E_SOURCE" "${sftp[@]}"

echo "== full backup"
t0=$(date +%s); "$RBACKUP" "${db[@]}" backup --job e2e 2>&1 | tr '\r' '\n' | grep -a -E "done in|finalized"; t1=$(date +%s)
echo "   wall: $((t1 - t0)) s"
snap=$("$RBACKUP" "${db[@]}" snapshots --job e2e | grep -o -E '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}' | head -1)
echo "   snapshot: $snap"

echo "== restore one file: $E2E_FILE"
"$RBACKUP" "${db[@]}" restore --job e2e --snapshot "$snap" --path "$E2E_FILE" --to "$WORK/r-file"
cmp "$E2E_SOURCE/$E2E_FILE" "$WORK/r-file/$src_name/$E2E_FILE" && echo "   OK byte-identical"
n=$(find "$WORK/r-file" -type f | wc -l); echo "   files restored: $n (expected 1)"; [ "$n" -eq 1 ]

echo "== restore one directory: $E2E_DIR"
"$RBACKUP" "${db[@]}" restore --job e2e --snapshot "$snap" --path "$E2E_DIR" --to "$WORK/r-dir"
diff -r "$E2E_SOURCE/$E2E_DIR" "$WORK/r-dir/$src_name/$E2E_DIR" && echo "   OK directory byte-identical"
a=$(find "$E2E_SOURCE/$E2E_DIR" -type f | wc -l); b=$(find "$WORK/r-dir" -type f | wc -l)
echo "   files: source $a, restored $b"; [ "$a" -eq "$b" ]

echo "== full verify"
"$RBACKUP" "${db[@]}" verify --job e2e --full 2>&1 | tail -3

echo "== incremental rerun (unchanged)"
t0=$(date +%s); "$RBACKUP" "${db[@]}" backup --job e2e 2>&1 | tr '\r' '\n' | grep -a -E "finalized"; t1=$(date +%s)
echo "   wall: $((t1 - t0)) s"
echo "E2E PASSED"
