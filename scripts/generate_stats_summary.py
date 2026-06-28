#!/usr/bin/env python3
import json
import glob
import os
import sys

def main():
    stats_dir = os.environ.get("STATS_DIR")
    if not stats_dir:
        print("Error: STATS_DIR environment variable is not set.", file=sys.stderr)
        sys.exit(1)
        
    stats_files = glob.glob(os.path.join(stats_dir, "*.stats"))
    if not stats_files:
        print("No stats files found in the directory.")
        return

    summary = []
    summary.append("### ⚡ Performance stats summary")
    summary.append("| Example | Total Duration | Font Init | PTY Spawn | Execution | Captured Frames | Dropped Frames |")
    summary.append("| --- | --- | --- | --- | --- | --- | --- |")

    for f in sorted(stats_files):
        name = os.path.basename(f).replace(".stats", "")
        try:
            with open(f, "r") as fh:
                data = json.load(fh)
            tel = data.get("telemetry", {})
            wall = data.get("wall_ms", 0)
            font = tel.get("font_init", 0)
            pty = tel.get("pty_spawn", 0)
            exec_t = tel.get("runner_execution", 0)
            captured = data.get("captured_frames", 0)
            dropped = data.get("raw_frame_consumer_dropped_frames", 0)
            summary.append(f"| **{name}** | {wall}ms | {font}ms | {pty}ms | {exec_t}ms | {captured} | {dropped} |")
        except Exception as e:
            summary.append(f"| **{name}** | Error: {e} | | | | | |")

    summary_str = "\n".join(summary) + "\n"
    print(summary_str)
    
    step_summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
    if step_summary_path:
        with open(step_summary_path, "a") as out:
            out.write(summary_str)

if __name__ == "__main__":
    main()
