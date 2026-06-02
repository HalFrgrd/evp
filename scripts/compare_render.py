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
import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import xml.etree.ElementTree as ET
from pathlib import Path


DEFAULT_EVP_BIN = "./target/x86_64-unknown-linux-musl/release/evp"
FALLBACK_EVP_BIN = "./target/release/evp"
DEFAULT_VHS_IMAGE = "evp-vhs:latest"


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

def run_evp(evp: Path, tape: Path, out_gif: Path, out_svg: Path | None = None) -> float:
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
        cmd = [str(evp), str(tmp_path)]
        if out_svg:
            cmd.extend(["--output", str(out_gif), "--output", str(out_svg)])
        print(f"  evp: {shlex.join(cmd)}")
        _, elapsed = run(cmd)
    finally:
        tmp_path.unlink(missing_ok=True)

    return elapsed


def run_vhs(image: str, tape: Path, out_gif: Path) -> float:
    """Run VHS via Docker on *tape*, writing output to *out_gif*. Returns wall seconds."""
    # Build VHS image using docker bake to pick up any changes
    if image == "evp-vhs:latest":
        print("  Running docker buildx bake vhs --load...")
        subprocess.run(["docker", "buildx", "bake", "vhs", "--load"], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

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
        elif line.lstrip().startswith("Set Shell "):
            # Strip any shell arguments, since VHS only accepts the shell executable name
            parts = line.strip().split()
            if len(parts) >= 3:
                shell_name = parts[2]
                new_lines.append(f"Set Shell {shell_name}\n")
            else:
                new_lines.append(line)
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
        "input.tape",
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
# Perceptual Frame-by-Frame Diffing
# ---------------------------------------------------------------------------

def check_dependencies() -> None:
    try:
        from PIL import Image, ImageSequence
        from playwright.sync_api import sync_playwright
    except ImportError as e:
        sys.exit(
            f"Required dependency missing: {e.name}.\n"
            "Please install the required dependencies with:\n"
            "  .venv/bin/pip install playwright pillow"
        )


def extract_gif_frames(gif_path: Path) -> list[dict]:
    from PIL import Image, ImageSequence
    img = Image.open(gif_path)
    frames = []
    
    canvas = Image.new("RGBA", img.size, (0, 0, 0, 0))
    
    current_time_ms = 0
    for idx, gif_frame in enumerate(ImageSequence.Iterator(img)):
        disposal = getattr(gif_frame, "disposal_method", 0)
        current_frame = canvas.copy()
        
        frame_rgba = gif_frame.convert("RGBA")
        if gif_frame.tile:
            bbox = gif_frame.tile[0][1]
        else:
            bbox = (0, 0, img.width, img.height)
            
        current_frame.paste(frame_rgba, bbox, mask=frame_rgba)
        duration = gif_frame.info.get("duration", 100)
        
        frames.append({
            "image": current_frame,
            "duration": duration,
            "t_start": current_time_ms,
            "t_end": current_time_ms + duration,
            "index": idx
        })
        
        if disposal == 2:
            canvas.paste((0, 0, 0, 0), bbox)
        elif disposal == 3:
            pass
        else:
            canvas = current_frame.copy()
            
        current_time_ms += duration
        
    return frames


def get_svg_dimensions(svg_path: Path) -> tuple[int, int]:
    try:
        tree = ET.parse(svg_path)
        root = tree.getroot()
        width = int(float(root.attrib.get("width", 800)))
        height = int(float(root.attrib.get("height", 600)))
        return width, height
    except Exception as e:
        print(f"Warning: failed to parse SVG dimensions: {e}. Using fallback 800x600.")
        return 800, 600


def run_agent_image_diff(img1: Path, img2: Path, out_diff: Path | None = None) -> dict:
    cmd = ["agent-image-diff", str(img1), str(img2)]
    if out_diff:
        cmd.extend(["-o", str(out_diff)])
    
    res = subprocess.run(cmd, capture_output=True, text=True)
    try:
        return json.loads(res.stdout)
    except json.JSONDecodeError:
        return {
            "match": False,
            "diff_percentage": 100.0,
            "error": f"Failed to parse JSON. stdout: {res.stdout}, stderr: {res.stderr}"
        }


def find_frame_at_time(frames: list[dict], t_ms: float) -> dict:
    if not frames:
        raise ValueError("Frames list is empty")
    total_duration = frames[-1]["t_end"]
    t_ms = max(0.0, min(t_ms, total_duration - 1.0))
    for frame in frames:
        if frame["t_start"] <= t_ms < frame["t_end"]:
            return frame
    return frames[-1]


def save_comparison_grid(
    evp_svg_png: Path,
    evp_gif_png: Path,
    vhs_gif_png: Path,
    diff_gif_svg_png: Path,
    diff_gif_vhs_png: Path,
    diff_svg_vhs_png: Path,
    out_path: Path,
) -> None:
    from PIL import Image, ImageDraw
    
    with Image.open(evp_svg_png) as img_svg, \
         Image.open(evp_gif_png) as img_evp_gif, \
         Image.open(vhs_gif_png) as img_vhs_gif, \
         Image.open(diff_gif_svg_png) as img_diff_gif_svg, \
         Image.open(diff_gif_vhs_png) as img_diff_gif_vhs, \
         Image.open(diff_svg_vhs_png) as img_diff_svg_vhs:
         
        width, height = img_svg.size
        h_header = 30
        
        grid = Image.new("RGBA", (width * 3, (height + h_header) * 2), (30, 30, 30, 255))
        
        # Row 0 (Frames)
        grid.paste(img_svg, (0, h_header))
        grid.paste(img_evp_gif, (width, h_header))
        grid.paste(img_vhs_gif, (width * 2, h_header))
        
        # Row 1 (Diffs)
        grid.paste(img_diff_gif_svg, (0, height + 2 * h_header))
        grid.paste(img_diff_gif_vhs, (width, height + 2 * h_header))
        grid.paste(img_diff_svg_vhs, (width * 2, height + 2 * h_header))
        
        # Draw labels
        draw = ImageDraw.Draw(grid)
        draw.text((10, 8), "EVP SVG", fill=(255, 255, 255, 255))
        draw.text((width + 10, 8), "EVP GIF", fill=(255, 255, 255, 255))
        draw.text((width * 2 + 10, 8), "VHS GIF", fill=(255, 255, 255, 255))
        
        draw.text((10, height + h_header + 8), "Diff: EVP GIF (red) vs EVP SVG (blue)", fill=(255, 255, 255, 255))
        draw.text((width + 10, height + h_header + 8), "Diff: EVP GIF (red) vs VHS GIF (blue)", fill=(255, 255, 255, 255))
        draw.text((width * 2 + 10, height + h_header + 8), "Diff: EVP SVG (red) vs VHS GIF (blue)", fill=(255, 255, 255, 255))
        
        grid.save(out_path)


def compare_renders_frame_by_frame(
    tape: Path,
    evp_gif: Path,
    evp_svg: Path,
    vhs_gif: Path,
    out_dir: Path,
    diff_threshold: float,
) -> None:
    check_dependencies()
    from playwright.sync_api import sync_playwright

    print("[diff] starting frame-by-frame perceptual diffs...")

    print("  Extracting frames from evp.gif...")
    evp_frames = extract_gif_frames(evp_gif)
    print(f"    Extracted {len(evp_frames)} frames.")

    print("  Extracting frames from vhs.gif...")
    vhs_frames = extract_gif_frames(vhs_gif)
    print(f"    Extracted {len(vhs_frames)} frames.")

    if not evp_frames or not vhs_frames:
        print("Error: No frames extracted from GIFs.")
        return

    width, height = get_svg_dimensions(evp_svg)
    print(f"  SVG dimensions parsed: {width}x{height}")

    temp_frames_dir = out_dir / "_temp_frames"
    temp_frames_dir.mkdir(parents=True, exist_ok=True)

    results = []
    discrepancies_count = 0

    max_diffs = {
        "evp_gif_vs_evp_svg": {"val": 0.0, "frame": 0},
        "evp_gif_vs_vhs_gif": {"val": 0.0, "frame": 0},
        "evp_svg_vs_vhs_gif": {"val": 0.0, "frame": 0},
    }
    
    sum_diffs = {
        "evp_gif_vs_evp_svg": 0.0,
        "evp_gif_vs_vhs_gif": 0.0,
        "evp_svg_vs_vhs_gif": 0.0,
    }

    with sync_playwright() as p:
        print("  Launching headless browser for SVG rendering...")
        browser = p.chromium.launch(
            executable_path="/usr/bin/google-chrome",
            args=["--headless", "--disable-gpu", "--no-sandbox"]
        )
        page = browser.new_page(viewport={"width": width, "height": height})
        page.goto(f"file://{evp_svg.resolve()}")
        
        page.evaluate("const svg = document.querySelector('svg'); if (svg) svg.pauseAnimations();")

        for idx, evp_frame in enumerate(evp_frames):
            t_ms = evp_frame["t_start"]
            t_sec = t_ms / 1000.0

            page.evaluate(f"const svg = document.querySelector('svg'); if (svg) svg.setCurrentTime({t_sec});")
            page.wait_for_timeout(20)

            evp_gif_png = temp_frames_dir / f"evp_gif_{idx}.png"
            evp_frame["image"].save(evp_gif_png)

            evp_svg_png = temp_frames_dir / f"evp_svg_{idx}.png"
            page.screenshot(path=str(evp_svg_png))

            vhs_frame = find_frame_at_time(vhs_frames, t_ms)
            vhs_gif_png = temp_frames_dir / f"vhs_gif_{idx}.png"
            vhs_frame["image"].save(vhs_gif_png)

            diff_paths = {
                "evp_gif_vs_evp_svg": (evp_gif_png, evp_svg_png),
                "evp_gif_vs_vhs_gif": (evp_gif_png, vhs_gif_png),
                "evp_svg_vs_vhs_gif": (evp_svg_png, vhs_gif_png),
            }

            frame_diffs = {}
            has_discrepancy = False

            for diff_name, (img1, img2) in diff_paths.items():
                temp_diff_png = temp_frames_dir / f"temp_diff_{diff_name}_{idx}.png"
                res = run_agent_image_diff(img1, img2, None)
                
                # Render custom diff: grayscale intensities, red (img1 > img2), blue (img2 > img1)
                from PIL import Image, ImageChops
                i1 = Image.open(img1).convert("L")
                i2 = Image.open(img2).convert("L")
                r = ImageChops.subtract(i1, i2)
                b = ImageChops.subtract(i2, i1)
                g = Image.new("L", i1.size, 0)
                diff_img = Image.merge("RGB", (r, g, b))
                diff_img.save(temp_diff_png)
                
                diff_pct = res.get("diff_percentage", 100.0)
                matched = res.get("match", False)

                frame_diffs[diff_name] = {
                    "match": matched,
                    "diff_percentage": diff_pct
                }

                sum_diffs[diff_name] += diff_pct
                if diff_pct > max_diffs[diff_name]["val"]:
                    max_diffs[diff_name]["val"] = diff_pct
                    max_diffs[diff_name]["frame"] = idx

                if diff_pct > diff_threshold:
                    has_discrepancy = True

            results.append({
                "frame_index": idx,
                "timestamp_ms": t_ms,
                "diffs": frame_diffs
            })

            if has_discrepancy:
                discrepancies_count += 1
                
                grid_out = out_dir / f"frame_{idx:03d}_t{t_ms:05d}ms_comparison.png"
                save_comparison_grid(
                    evp_svg_png=evp_svg_png,
                    evp_gif_png=evp_gif_png,
                    vhs_gif_png=vhs_gif_png,
                    diff_gif_svg_png=temp_frames_dir / f"temp_diff_evp_gif_vs_evp_svg_{idx}.png",
                    diff_gif_vhs_png=temp_frames_dir / f"temp_diff_evp_gif_vs_vhs_gif_{idx}.png",
                    diff_svg_vhs_png=temp_frames_dir / f"temp_diff_evp_svg_vs_vhs_gif_{idx}.png",
                    out_path=grid_out
                )

        browser.close()

    shutil.rmtree(temp_frames_dir, ignore_errors=True)

    num_frames = len(evp_frames)
    avg_diffs = {k: v / num_frames for k, v in sum_diffs.items()}

    report_data = {
        "tape": tape.name,
        "total_frames": num_frames,
        "diff_threshold": diff_threshold,
        "discrepant_frames_count": discrepancies_count,
        "summary": {
            k: {
                "avg_diff": round(avg_diffs[k], 3),
                "max_diff": round(max_diffs[k]["val"], 3),
                "max_diff_frame": max_diffs[k]["frame"]
            }
            for k in avg_diffs.keys()
        },
        "frames": results
    }

    report_path = out_dir / "report.json"
    report_path.write_text(json.dumps(report_data, indent=2))
    print(f"  Perceptual diff report written to {report_path}")

    print()
    print("=== Perceptual Diff Summary ===")
    print(f"  Compared {num_frames} frames with threshold {diff_threshold}%")
    print(f"  Frames exceeding threshold: {discrepancies_count}")
    print("  Average differences:")
    for k, v in avg_diffs.items():
        print(f"    {k:<20}: {v:.3f}% (max: {max_diffs[k]['val']:.3f}% at frame {max_diffs[k]['frame']})")
    print()
    if discrepancies_count > 0:
        print(f"  Visual diff PNGs saved to: {out_dir}")
    print("===============================")


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main() -> None:
    parser = argparse.ArgumentParser(
        description="Run a tape file through evp and VHS and compare outputs."
    )
    parser.add_argument("tape", type=Path, help="Path to the .tape file")
    parser.add_argument(
        "-o",
        "--out-dir",
        type=Path,
        default=None,
        help="Directory for output files (default: <tape_dir>/compare-out/)",
    )
    parser.add_argument(
        "-m",
        "--mode",
        choices=["diff", "metadata"],
        default="diff",
        help="Comparison mode (default: diff)",
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
        "--skip",
        choices=["evp", "vhs"],
        default=None,
        help="Skip execution of the specified engine",
    )
    parser.add_argument(
        "--diff-threshold",
        type=float,
        default=0.1,
        help="Perceptual diff percentage threshold (default: 0.1%%)",
    )
    args = parser.parse_args()

    tape = args.tape.resolve()
    if not tape.exists():
        sys.exit(f"tape not found: {tape}")

    out_dir = (args.out_dir or tape.parent / "compare-out").resolve()
    out_dir.mkdir(parents=True, exist_ok=True)

    evp_gif = out_dir / "evp.gif"
    evp_svg = out_dir / "evp.svg"
    vhs_gif = out_dir / "vhs.gif"

    evp_elapsed = 0.0
    vhs_elapsed = 0.0

    run_mode_diff = (args.mode == "diff")
    skip_evp = (args.skip == "evp")
    skip_vhs = (args.skip == "vhs")

    if run_mode_diff:
        if skip_evp or skip_vhs:
            sys.exit("Error: Cannot run frame-by-frame perceptual diffs if either evp or vhs is skipped.")

    if not skip_evp:
        evp_bin = find_evp(args.evp)
        print(f"[evp] running {tape.name} …")
        if run_mode_diff:
            evp_elapsed = run_evp(evp_bin, tape, evp_gif, evp_svg)
        else:
            evp_elapsed = run_evp(evp_bin, tape, evp_gif)
        print(f"[evp] done in {evp_elapsed:.2f}s → {evp_gif}")

    if not skip_vhs:
        if not docker_available():
            sys.exit("docker not found on PATH — cannot run VHS step.")
        print(f"[vhs] running {tape.name} via Docker ({args.vhs_image}) …")
        vhs_elapsed = run_vhs(args.vhs_image, tape, vhs_gif)
        print(f"[vhs] done in {vhs_elapsed:.2f}s → {vhs_gif}")

    if not skip_evp or not skip_vhs:
        report(tape, evp_gif, vhs_gif, evp_elapsed, vhs_elapsed)

    if run_mode_diff and not skip_evp and not skip_vhs:
        compare_renders_frame_by_frame(tape, evp_gif, evp_svg, vhs_gif, out_dir, args.diff_threshold)


if __name__ == "__main__":
    main()
