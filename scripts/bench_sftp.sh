#!/usr/bin/env bash
# Reproducible SFTP backup benchmark (Git Bash on Windows, or any POSIX shell).
#
# Runs one fresh full backup per (corpus, durability mode, upload workers)
# combination against a throwaway repository on the server, then a second
# (unchanged) run to measure the dedup path, and prints one result row each.
#
# Required environment:
#   BENCH_HOST, BENCH_USER, BENCH_KEY   SSH target and private key
#   BENCH_ROOT                          scratch directory on the server (wiped per case)
#   BENCH_CORPUS                        local folder made by scripts/make_bench_corpus.py
# Optional:
#   RBACKUP          path of the rbackup binary (default target/release/rbackup.exe)
#   BENCH_CORPORA    corpora to run (default "documents mixed")
#   BENCH_MODES      <durability>-<transport> modes: durability fsync|batched, transport
#                    chan (writers share one SSH connection) | conn (one connection per writer)
#                    (default "fsync-chan batched-chan batched-conn")
#   BENCH_WORKERS    upload worker counts (default "4 8")
#
# A throwaway passphrase is used; repositories are deleted after each case.
set -euo pipefail
export MSYS_NO_PATHCONV=1   # keep /remote/paths intact (AGENTS.md lesson 2026-11-08)

: "${BENCH_HOST:?}" "${BENCH_USER:?}" "${BENCH_KEY:?}" "${BENCH_ROOT:?}" "${BENCH_CORPUS:?}"
RBACKUP="${RBACKUP:-target/release/rbackup.exe}"
CORPORA="${BENCH_CORPORA:-documents mixed}"
MODES="${BENCH_MODES:-fsync-chan batched-chan batched-conn}"
WORKERS="${BENCH_WORKERS:-4 8}"
export RBACKUP_PASSPHRASE="bench-only-throwaway"
SSH=(ssh -i "$BENCH_KEY" -o IdentitiesOnly=yes -o BatchMode=yes "$BENCH_USER@$BENCH_HOST")
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

printf '%-10s %-14s %-3s %9s %10s %9s\n' corpus mode w full_s MiB/s rerun_s
for corpus in $CORPORA; do
  for mode in $MODES; do
    for w in $WORKERS; do
      remote="$BENCH_ROOT/$corpus-$mode-$w"
      "${SSH[@]}" "rm -rf '$remote'"
      db="$WORK/$corpus-$mode-$w.duckdb"
      common=(--db "$db")
      sftp=(--sftp-host "$BENCH_HOST" --sftp-user "$BENCH_USER" --sftp-key "$BENCH_KEY" --sftp-root "$remote")
      "$RBACKUP" "${common[@]}" repo init "${sftp[@]}" >/dev/null
      "$RBACKUP" "${common[@]}" jobs add --name bench --source "$BENCH_CORPUS/$corpus" "${sftp[@]}" >/dev/null
      case "$mode" in fsync*) export RBACKUP_SFTP_DURABILITY=fsync ;; *) unset RBACKUP_SFTP_DURABILITY ;; esac
      case "$mode" in *-chan) export RBACKUP_SFTP_WRITERS=channel ;; *) export RBACKUP_SFTP_WRITERS=connection ;; esac
      export RBACKUP_UPLOAD_WORKERS="$w"
      bytes=$(du -sb "$BENCH_CORPUS/$corpus" | cut -f1)
      t0=$(date +%s.%N); "$RBACKUP" "${common[@]}" backup --job bench >/dev/null 2>&1; t1=$(date +%s.%N)
      "$RBACKUP" "${common[@]}" backup --job bench >/dev/null 2>&1; t2=$(date +%s.%N)
      full=$(awk -v a="$t0" -v b="$t1" 'BEGIN{printf "%.1f", b-a}')
      rerun=$(awk -v a="$t1" -v b="$t2" 'BEGIN{printf "%.1f", b-a}')
      mibs=$(awk -v n="$bytes" -v s="$full" 'BEGIN{printf "%.2f", n/1048576/s}')
      printf '%-10s %-14s %-3s %9.1f %10s %9.1f\n' "$corpus" "$mode" "$w" "$full" "$mibs" "$rerun"
      "${SSH[@]}" "rm -rf '$remote'"
    done
  done
done
