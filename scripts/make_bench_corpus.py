"""Generates the deterministic SFTP/USB benchmark corpora.

Two corpora (same bytes on every run, fixed seeds):
- mixed:     2 x 256 MiB incompressible + 2 x 256 MiB compressible + 800 files of 8-32 KiB
- documents: 4000 files of 4-128 KiB (half compressible text-like, half random) in 40 folders,
             the shape of a real "Documents" folder (many small files)

Usage (from the repository root):
    uv run --no-project python scripts/make_bench_corpus.py <out_dir> [mixed|documents|all]
"""

import random
import sys
from pathlib import Path

MIB = 1024 * 1024


def rand_bytes(rng: random.Random, n: int) -> bytes:
    return rng.randbytes(n)


def text_bytes(rng: random.Random, n: int) -> bytes:
    words = [b"backup", b"archive", b"ledger", b"invoice", b"report", b"draft", b"notes", b"photo",
             b"2026", b"rust", b"chunk", b"vault", b"letter", b"budget", b"contract", b"meeting"]
    out = bytearray()
    while len(out) < n:
        out += rng.choice(words) + (b"\n" if rng.random() < 0.1 else b" ")
    return bytes(out[:n])


def write(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)


def mixed(out: Path) -> None:
    rng = random.Random(1001)
    for i in range(2):
        write(out / f"big/random-{i}.bin", rand_bytes(rng, 256 * MIB))
        write(out / f"big/text-{i}.txt", text_bytes(rng, 256 * MIB))
    for i in range(800):
        write(out / f"small/s{i:04}.bin", rand_bytes(rng, rng.randint(8, 32) * 1024))


def documents(out: Path) -> None:
    rng = random.Random(2002)
    for i in range(4000):
        size = rng.randint(4, 128) * 1024
        data = text_bytes(rng, size) if i % 2 == 0 else rand_bytes(rng, size)
        write(out / f"folder-{i % 40:02}/doc-{i:05}.dat", data)


def main() -> None:
    out = Path(sys.argv[1])
    which = sys.argv[2] if len(sys.argv) > 2 else "all"
    if which in ("mixed", "all"):
        mixed(out / "mixed")
    if which in ("documents", "all"):
        documents(out / "documents")
    total = sum(f.stat().st_size for f in out.rglob("*") if f.is_file())
    count = sum(1 for f in out.rglob("*") if f.is_file())
    print(f"corpus ready in {out}: {count} files, {total / MIB:.1f} MiB")


if __name__ == "__main__":
    main()
