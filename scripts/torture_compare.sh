#!/usr/bin/env bash
# Compare the GIFs and timing data produced by the evp and VHS torture
# runs and emit a single Markdown report suitable for an actions
# artifact.
#
# Inputs (all required):
#   $1 = path to the evp gif
#   $2 = path to the evp report.txt (produced by torture_benchmark)
#   $3 = path to the vhs gif
#   $4 = path to the vhs timing log (key=value lines)
#   $5 = path to write the markdown report to
#
# All paths must already exist except the output report.

set -euo pipefail

if [[ $# -ne 5 ]]; then
    echo "usage: $0 <evp.gif> <evp.report> <vhs.gif> <vhs.report> <out.md>" >&2
    exit 64
fi

evp_gif=$1
evp_report=$2
vhs_gif=$3
vhs_report=$4
out_md=$5

if [[ ! -s "$evp_gif" ]]; then
    echo "missing or empty evp gif: $evp_gif" >&2
    exit 66
fi
if [[ ! -s "$vhs_gif" ]]; then
    echo "missing or empty vhs gif: $vhs_gif" >&2
    exit 66
fi

human_bytes() {
    local b=$1
    awk -v b="$b" 'BEGIN {
        split("B KB MB GB", u);
        i = 1;
        while (b >= 1024 && i < 4) { b /= 1024; i++ }
        printf("%.1f %s", b, u[i]);
    }'
}

bytes_or_zero() {
    if [[ -f "$1" ]]; then
        stat -c %s "$1"
    else
        echo 0
    fi
}

md5_or_missing() {
    if [[ -f "$1" ]]; then
        md5sum "$1" | awk '{print $1}'
    else
        echo "(missing)"
    fi
}

evp_bytes=$(bytes_or_zero "$evp_gif")
vhs_bytes=$(bytes_or_zero "$vhs_gif")
evp_md5=$(md5_or_missing "$evp_gif")
vhs_md5=$(md5_or_missing "$vhs_gif")

# Pull a couple of fields from the evp report for the summary table.
extract() {
    # extract <file> <key>
    awk -F'=' -v k="$2" '
        $1 ~ "^[[:space:]]*"k"[[:space:]]*$" { sub(/^[[:space:]]+/, "", $2); print $2; exit }
    ' "$1"
}

evp_wall=$(extract "$evp_report" "wall_ms")
evp_dropped=$(extract "$evp_report" "dropped_capture")
evp_max_q1=$(extract "$evp_report" "max_capture_queue")
evp_max_q2=$(extract "$evp_report" "max_renderer_queue")
evp_result=$(extract "$evp_report" "result")
evp_cpu=$(extract "$evp_report" "cpu_affinity")
vhs_wall=$(extract "$vhs_report" "wall_ms")
vhs_cpu=$(extract "$vhs_report" "cpu_affinity")

ratio="n/a"
if [[ "$vhs_wall" =~ ^[0-9]+$ && "$evp_wall" =~ ^[0-9]+$ && "$evp_wall" -gt 0 ]]; then
    ratio=$(awk -v v="$vhs_wall" -v e="$evp_wall" 'BEGIN { printf("%.2fx", v/e) }')
fi

# Run the GIF frame analyzer on each output. The analyzer is stdlib-only
# Python so this works in any CI image with python3 installed. If the
# analyzer can't run (missing gif, missing python) we still produce the
# rest of the report.
script_dir=$(cd "$(dirname "$0")" && pwd)
analyzer="$script_dir/gif_frame_analyzer.py"
# Expected fps for the comparison. Override by exporting TORTURE_FPS
# before invoking this script. Defaults to 60, matching torture.tape.
fps=${TORTURE_FPS:-60}

run_analyzer() {
    # run_analyzer <gif> <label>
    local gif=$1
    local label=$2
    if [[ ! -f "$gif" ]]; then
        echo "(no $label gif at $gif — analysis skipped)"
        return
    fi
    if ! command -v python3 >/dev/null 2>&1; then
        echo "(python3 not available — $label gif analysis skipped)"
        return
    fi
    python3 "$analyzer" "$gif" --fps "$fps" --label "$label" 2>&1 \
        || echo "(analyzer failed for $label)"
}

evp_analysis=$(run_analyzer "$evp_gif" evp)
vhs_analysis=$(run_analyzer "$vhs_gif" vhs)

# Pull a couple of headline numbers out of each analysis for the
# summary table. Falls back to "?" if the analysis didn't run.
analysis_get() {
    # analysis_get <analysis-text> <key>
    awk -F'=' -v k="$2" '
        $1 ~ "^[[:space:]]*"k"[[:space:]]*$" { sub(/^[[:space:]]+/, "", $2); print $2; exit }
    ' <<<"$1"
}
evp_frames=$(analysis_get "$evp_analysis" "frame_count")
evp_skipped=$(analysis_get "$evp_analysis" "skipped_frames_est")
vhs_frames=$(analysis_get "$vhs_analysis" "frame_count")
vhs_skipped=$(analysis_get "$vhs_analysis" "skipped_frames_est")

cat >"$out_md" <<MD
# evp vs VHS torture benchmark

Both runs were pinned to a single CPU core (\`taskset -c 0\` for evp,
\`--cpuset-cpus=0 --cpus=1\` for the VHS docker container) and rendered
the same \`examples/torture.tape\` script: a 100×30 grid at 60 fps,
typing at ~125 chars/sec (≥2 keystrokes per captured frame) for ~10 s,
where every keystroke triggers a full-screen redraw with random ASCII
+ random fg/bg + random modifiers in every cell.

## Summary

| metric | evp | VHS |
|---|---|---|
| wall time (ms) | ${evp_wall:-?} | ${vhs_wall:-?} |
| gif size | $(human_bytes "$evp_bytes") ($evp_bytes B) | $(human_bytes "$vhs_bytes") ($vhs_bytes B) |
| md5 | \`$evp_md5\` | \`$vhs_md5\` |
| gif frames | ${evp_frames:-?} | ${vhs_frames:-?} |
| skipped frames (est. @ ${fps} fps) | ${evp_skipped:-?} | ${vhs_skipped:-?} |
| cpu affinity | $evp_cpu | $vhs_cpu |

VHS wall-clock / evp wall-clock = **${ratio}** (>1 means evp is faster).

## evp pipeline health

- dropped capture frames: ${evp_dropped:-?}
- max runner→encoder queue: ${evp_max_q1:-?}
- max encoder→renderer queue: ${evp_max_q2:-?}
- pass/fail (>5 % missed = fail): **${evp_result:-?}**

## evp gif frame analysis

\`\`\`
$evp_analysis
\`\`\`

## VHS gif frame analysis

\`\`\`
$vhs_analysis
\`\`\`

## evp full report

\`\`\`
$(cat "$evp_report")
\`\`\`

## VHS full report

\`\`\`
$(cat "$vhs_report")
\`\`\`
MD

echo "wrote $out_md"
