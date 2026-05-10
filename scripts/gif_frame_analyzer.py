#!/usr/bin/env python3
"""Walk a GIF's frame timestamps and report whether frames were skipped.

Pure-stdlib GIF parser (no Pillow / imageio dependency) so it can be
dropped into any CI image without a `pip install` step. Decodes only
the *control* metadata of each frame — image data is skipped — which
is enough to recover the per-frame `delay_ms` field encoded in each
Graphic Control Extension.

Usage:

    gif_frame_analyzer.py <path.gif> [--fps N] [--json] [--label NAME]

The analyzer:

  * counts frames,
  * sums per-frame delays into a total animation duration,
  * given an `--fps`, computes the *expected* frame count and counts
    frames whose delay is more than 1.5× the expected interval (these
    are "long" / "skipped" frames — the GIF effectively dropped one or
    more frames at that point),
  * prints a short text report (or `--json` for machine consumption).

GIFs encode `delay` in centiseconds (10 ms units) per the GIF89a spec;
many encoders clamp to 2 cs (= 50 fps maximum effective rate). The
analyzer reports the raw delays in ms so the clamping is visible.
"""

from __future__ import annotations

import argparse
import json
import struct
import sys
from dataclasses import dataclass


GIF_HEADER = b"GIF87a", b"GIF89a"


@dataclass
class FrameInfo:
    index: int
    delay_ms: int  # raw delay encoded in the Graphic Control Extension


def parse_gif(path: str) -> list[FrameInfo]:
    """Return per-frame metadata for `path`.

    Raises ValueError if the file isn't a syntactically valid GIF.
    """
    with open(path, "rb") as f:
        data = f.read()

    if not any(data.startswith(h) for h in GIF_HEADER):
        raise ValueError(f"{path}: not a GIF (header={data[:6]!r})")

    pos = 6
    # Logical Screen Descriptor.
    if len(data) < pos + 7:
        raise ValueError("truncated logical screen descriptor")
    _w, _h, packed, _bg, _ar = struct.unpack_from("<HHBBB", data, pos)
    pos += 7
    if packed & 0x80:
        # Global Color Table follows: 3 * 2^(n+1) bytes.
        gct_size = 3 * (1 << ((packed & 0x07) + 1))
        pos += gct_size

    frames: list[FrameInfo] = []
    pending_delay_ms = 0  # most recent Graphic Control Extension delay

    while pos < len(data):
        intro = data[pos]
        pos += 1
        if intro == 0x3B:  # Trailer
            break
        if intro == 0x21:  # Extension Introducer
            label = data[pos]
            pos += 1
            # Read sub-blocks; a Graphic Control Extension carries the delay.
            sub_blocks: list[bytes] = []
            while True:
                size = data[pos]
                pos += 1
                if size == 0:
                    break
                sub_blocks.append(data[pos : pos + size])
                pos += size
            if label == 0xF9 and sub_blocks:
                # Graphic Control Extension. First sub-block is 4 bytes:
                # packed, delay_lo, delay_hi, transparent_index.
                blk = sub_blocks[0]
                if len(blk) >= 3:
                    delay_cs = blk[1] | (blk[2] << 8)
                    pending_delay_ms = delay_cs * 10
        elif intro == 0x2C:  # Image Descriptor
            # 9 bytes: left, top, w, h, packed.
            if len(data) < pos + 9:
                raise ValueError("truncated image descriptor")
            _l, _t, _iw, _ih, ipacked = struct.unpack_from("<HHHHB", data, pos)
            pos += 9
            if ipacked & 0x80:
                lct_size = 3 * (1 << ((ipacked & 0x07) + 1))
                pos += lct_size
            # LZW minimum code size byte, then sub-blocks of image data.
            pos += 1  # LZW min code size
            while pos < len(data):
                size = data[pos]
                pos += 1
                if size == 0:
                    break
                pos += size
            frames.append(FrameInfo(index=len(frames), delay_ms=pending_delay_ms))
            pending_delay_ms = 0
        else:
            raise ValueError(
                f"unexpected block introducer 0x{intro:02x} at offset {pos - 1}"
            )

    return frames


def analyze(
    frames: list[FrameInfo],
    expected_fps: float | None,
) -> dict:
    """Reduce per-frame timings into a summary dict suitable for printing."""
    delays = [f.delay_ms for f in frames]
    total_ms = sum(delays)
    nonzero = [d for d in delays if d > 0]
    summary: dict = {
        "frame_count": len(frames),
        "total_duration_ms": total_ms,
        "min_delay_ms": min(delays) if delays else 0,
        "max_delay_ms": max(delays) if delays else 0,
        "avg_delay_ms": (sum(nonzero) / len(nonzero)) if nonzero else 0.0,
        "zero_delay_frames": sum(1 for d in delays if d == 0),
    }
    if expected_fps and expected_fps > 0:
        expected_interval_ms = 1000.0 / expected_fps
        # A "long" frame is one that took noticeably longer than the
        # expected per-frame interval — i.e. one or more frames were
        # effectively dropped at that boundary. 1.5× is the same
        # threshold most browser/devtools "long task" reports use.
        long_threshold_ms = expected_interval_ms * 1.5
        long_frames = [d for d in delays if d > long_threshold_ms]
        # Estimate how many "wall-clock" frames were skipped: each long
        # delay accounts for `round(d / expected_interval) - 1` skips.
        skipped_estimate = 0
        for d in delays:
            slots = round(d / expected_interval_ms) if expected_interval_ms else 1
            if slots > 1:
                skipped_estimate += slots - 1
        expected_frame_count = (
            round(total_ms / expected_interval_ms) if total_ms else 0
        )
        skipped_pct = (
            (skipped_estimate / expected_frame_count * 100.0)
            if expected_frame_count
            else 0.0
        )
        summary.update(
            {
                "expected_fps": expected_fps,
                "expected_interval_ms": round(expected_interval_ms, 3),
                "long_frame_threshold_ms": round(long_threshold_ms, 3),
                "long_frame_count": len(long_frames),
                "skipped_frames_estimate": skipped_estimate,
                "expected_frame_count": expected_frame_count,
                "skipped_pct": round(skipped_pct, 2),
            }
        )
    return summary


def format_text(summary: dict, label: str | None) -> str:
    out = []
    title = f"=== gif frame analysis ({label}) ===" if label else "=== gif frame analysis ==="
    out.append(title)
    out.append(f"frame_count          = {summary['frame_count']}")
    out.append(f"total_duration_ms    = {summary['total_duration_ms']}")
    out.append(f"min_delay_ms         = {summary['min_delay_ms']}")
    out.append(f"max_delay_ms         = {summary['max_delay_ms']}")
    out.append(f"avg_delay_ms         = {summary['avg_delay_ms']:.2f}")
    out.append(f"zero_delay_frames    = {summary['zero_delay_frames']}")
    if "expected_fps" in summary:
        out.append(f"expected_fps         = {summary['expected_fps']}")
        out.append(f"expected_interval_ms = {summary['expected_interval_ms']}")
        out.append(f"expected_frame_count = {summary['expected_frame_count']}")
        out.append(
            f"long_frame_threshold = {summary['long_frame_threshold_ms']} ms"
        )
        out.append(f"long_frame_count     = {summary['long_frame_count']}")
        out.append(
            f"skipped_frames_est   = {summary['skipped_frames_estimate']} "
            f"({summary['skipped_pct']:.2f}%)"
        )
    return "\n".join(out) + "\n"


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    p.add_argument("gif", help="Path to a GIF file")
    p.add_argument(
        "--fps",
        type=float,
        default=None,
        help="Expected framerate; enables skipped-frame estimation.",
    )
    p.add_argument(
        "--json",
        action="store_true",
        help="Emit a machine-readable JSON report instead of plain text.",
    )
    p.add_argument(
        "--label",
        default=None,
        help="Optional label included in the text report header.",
    )
    args = p.parse_args(argv)

    try:
        frames = parse_gif(args.gif)
    except (OSError, ValueError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 2

    summary = analyze(frames, args.fps)
    if args.json:
        json.dump(summary, sys.stdout, indent=2)
        sys.stdout.write("\n")
    else:
        sys.stdout.write(format_text(summary, args.label))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
