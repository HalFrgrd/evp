#!/usr/bin/env python3
"""Run a tape file through both evp and VHS (via Docker) and compare the outputs.

Usage:
    python3 scripts/compare_render.py <tape_file> [options]

Examples:
    python3 scripts/compare_render.py examples/hello.tape
    python3 scripts/compare_render.py scripts/stress_test.tape --out-dir /tmp/cmp
    python3 scripts/compare_render.py examples/hello.tape --evp ./target/release/evp
    python3 scripts/compare_render.py examples/hello.tape --vhs-image ghcr.io/charmbracelet/vhs:latest

The script:
  1. Runs `evp <tape>` and records wall-clock time.
  2. Runs `docker run ... vhs <tape>` and records wall-clock time.
  3. Prints a side-by-side summary (file sizes, timings, MD5s).

Both GIFs are written to <out-dir>/evp.gif and <out-dir>/vhs.gif.

Requirements:
  - evp binary (built with `cargo build --release --bin evp`).
  - Docker with the VHS image available (pulled automatically if missing).
  - Python 3.9+.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path


DEFAULT_EVP_BIN = "./target/x86_64-unknown-linux-musl/release/evp"
FALLBACK_EVP_BIN = "./target/release/evp"
DEFAULT_VHS_IMAGE = "ghcr.io/charmbracelet/vhs:latest"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def human_bytes(n: int) -> str:
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024:
            return f"{n:.1f} {unit}"
        n /= 1024
    return f"{n:.1f} TB"


def md5(path: Path) -> str:
    h = hashlib.md5()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def run(cmd: list[str], *, env: dict | None = None, check: bool = True) -> tuple[int, float]:
    """Run *cmd*, return (returncode, wall_seconds)."""
    merged_env = {**os.environ, **(env or {})}
    t0 = time.perf_counter()
    result = subprocess.run(cmd, env=merged_env)
    elapsed = time.perf_counter() - t0
    if check and result.returncode != 0:
        raise RuntimeError(
            f"command failed (rc={result.returncode}): {shlex.join(cmd)}"
        )
    return result.returncode, elapsed


def find_evp(hint: str) -> Path:
    p = Path(hint)
    if p.exists():
        return p
    fallback = Path(FALLBACK_EVP_BIN)
    if fallback.exists():
        return fallback
    found = shutil.which("evp")
    if found:
        return Path(found)
    sys.exit(
        f"evp binary not found at {hint!r} or on PATH.\n"
        "Build it first with: cargo build --release --bin evp"
    )


def docker_available() -> bool:
    return shutil.which("docker") is not None


# ---------------------------------------------------------------------------
# Runners
# ---------------------------------------------------------------------------

def run_evp(evp: Path, tape: Path, out_gif: Path) -> float:
    """Run evp on *tape*, writing output to *out_gif*. Returns wall seconds."""
    # Rewrite the `Output` directive in the tape so the GIF lands in our
    # chosen location, then feed the patched tape via stdin.
    tape_text = tape.read_text()

    # Replace any existing Output line, or prepend one if absent.
    lines = tape_text.splitlines(keepends=True)
    new_lines: list[str] = []
    replaced = False
    for line in lines:
        if line.lstrip().startswith("Output "):
            new_lines.append(f"Output {out_gif}\n")
            replaced = True
        else:
            new_lines.append(line)
    if not replaced:
        new_lines.insert(0, f"Output {out_gif}\n")

    patched = "".join(new_lines)

    with tempfile.NamedTemporaryFile(
        suffix=".tape", mode="w", delete=False, prefix="compare_evp_"
    ) as tmp:
        tmp.write(patched)
        tmp_path = Path(tmp.name)

    try:
        print(f"  evp: {evp} {tmp_path}")
        _, elapsed = run([str(evp), str(tmp_path)])
    finally:
        tmp_path.unlink(missing_ok=True)

    return elapsed


def run_vhs(image: str, tape: Path, out_gif: Path) -> float:
    """Run VHS via Docker on *tape*, writing output to *out_gif*. Returns wall seconds."""
    tape_text = tape.read_text()

    # VHS resolves Output relative to the working directory inside the
    # container (/work). We force the output name to "vhs_out.gif".
    lines = tape_text.splitlines(keepends=True)
    new_lines: list[str] = []
    replaced = False
    for line in lines:
        if line.lstrip().startswith("Output "):
            new_lines.append("Output vhs_out.gif\n")
            replaced = True
        else:
            new_lines.append(line)
    if not replaced:
        new_lines.insert(0, "Output vhs_out.gif\n")

    patched = "".join(new_lines)

    # Write the patched tape and a helper script into a temp dir that we
    # bind-mount into the container.
    work_dir = out_gif.parent / "_vhs_work"
    work_dir.mkdir(parents=True, exist_ok=True)
    patched_tape = work_dir / "input.tape"
    patched_tape.write_text(patched)

    # Copy any helper scripts referenced in the tape (stress_test_program.py etc.)
    tape_dir = tape.parent
    for dep in ("stress_test_program.py",):
        src = tape_dir / dep
        if src.exists():
            shutil.copy2(src, work_dir / dep)

    cmd = [
        "docker", "run", "--rm",
        "-v", f"{work_dir.resolve()}:/work",
        "-w", "/work",
        image,
        "vhs", "input.tape",
    ]
    print(f"  vhs: {shlex.join(cmd)}")
    _, elapsed = run(cmd)

    produced = work_dir / "vhs_out.gif"
    if not produced.exists():
        raise RuntimeError(
            f"VHS did not produce /work/vhs_out.gif — check Docker output above."
        )
    shutil.copy2(produced, out_gif)
    return elapsed


# ---------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------

def report(
    tape: Path,
    evp_gif: Path,
    vhs_gif: Path,
    evp_elapsed: float,
    vhs_elapsed: float,
) -> None:
    evp_ok = evp_gif.exists() and evp_gif.stat().st_size > 0
    vhs_ok = vhs_gif.exists() and vhs_gif.stat().st_size > 0

    evp_size = evp_gif.stat().st_size if evp_ok else 0
    vhs_size = vhs_gif.stat().st_size if vhs_ok else 0
    evp_md5 = md5(evp_gif) if evp_ok else "(missing)"
    vhs_md5 = md5(vhs_gif) if vhs_ok else "(missing)"

    col = 24

    def row(label: str, evp_val: str, vhs_val: str) -> None:
        print(f"  {label:<{col}} evp: {evp_val:<30}  vhs: {vhs_val}")

    print()
    print(f"=== compare_render: {tape.name} ===")
    row("output gif",     str(evp_gif), str(vhs_gif))
    row("size",           human_bytes(evp_size), human_bytes(vhs_size))
    row("wall time",      f"{evp_elapsed:.2f}s", f"{vhs_elapsed:.2f}s")
    row("md5",            evp_md5, vhs_md5)
    if evp_md5 == vhs_md5 and evp_ok and vhs_ok:
        print("  GIFs are byte-for-byte identical.")
    print()


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main() -> None:
    parser = argparse.ArgumentParser(
        description="Run a tape file through evp and VHS and compare outputs."
    )
    parser.add_argument("tape", type=Path, help="Path to the .tape file")
    parser.add_argument(
        "out_dir_pos",
        type=Path,
        nargs="?",
        default=None,
        metavar="OUT_DIR",
        help="Directory for output GIFs (positional shorthand for --out-dir)",
    )
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=None,
        help="Directory for output GIFs (default: <tape_dir>/compare-out/)",
    )
    parser.add_argument(
        "--evp",
        default=DEFAULT_EVP_BIN,
        help=f"Path to evp binary (default: {DEFAULT_EVP_BIN})",
    )
    parser.add_argument(
        "--vhs-image",
        default=DEFAULT_VHS_IMAGE,
        help=f"Docker image for VHS (default: {DEFAULT_VHS_IMAGE})",
    )
    parser.add_argument(
        "--skip-evp",
        action="store_true",
        help="Skip the evp run (only run VHS).",
    )
    parser.add_argument(
        "--skip-vhs",
        action="store_true",
        help="Skip the VHS run (only run evp).",
    )
    args = parser.parse_args()

    tape = args.tape.resolve()
    if not tape.exists():
        sys.exit(f"tape not found: {tape}")

    out_dir = (args.out_dir or args.out_dir_pos or tape.parent / "compare-out").resolve()
    out_dir.mkdir(parents=True, exist_ok=True)

    evp_gif = out_dir / "evp.gif"
    vhs_gif = out_dir / "vhs.gif"

    evp_elapsed = 0.0
    vhs_elapsed = 0.0

    if not args.skip_evp:
        evp_bin = find_evp(args.evp)
        print(f"[evp] running {tape.name} …")
        evp_elapsed = run_evp(evp_bin, tape, evp_gif)
        print(f"[evp] done in {evp_elapsed:.2f}s → {evp_gif}")

    if not args.skip_vhs:
        if not docker_available():
            sys.exit("docker not found on PATH — cannot run VHS step.")
        print(f"[vhs] running {tape.name} via Docker ({args.vhs_image}) …")
        vhs_elapsed = run_vhs(args.vhs_image, tape, vhs_gif)
        print(f"[vhs] done in {vhs_elapsed:.2f}s → {vhs_gif}")

    if not args.skip_evp or not args.skip_vhs:
        report(tape, evp_gif, vhs_gif, evp_elapsed, vhs_elapsed)


if __name__ == "__main__":
    main()
