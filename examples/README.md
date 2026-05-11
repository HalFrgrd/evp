# evp examples

This directory contains small `.tape` scripts that double as smoke tests
for the evp recorder, the GIF renderer and (eventually) the animated SVG
backend.

| Script | What it shows |
| --- | --- |
| [hello.tape](hello.tape) | Bare-minimum recording – type a single command. |
| [shell-tour.tape](shell-tour.tape) | Multiple commands paced with `Wait` instead of `Sleep`. |
| [keys.tape](keys.tape) | Modifier keys + line-editing (`Ctrl+U`). |
| [colors.tape](colors.tape) | ANSI SGR colour table – stresses cell diffing. |
| [progress.tape](progress.tape) | In-place line rewrites – stresses the diff path. |
| [stress_test.tape](stress_test.tape) | 100×30 grid at 60 fps where every keystroke triggers a full-screen random-cell repaint. Used by the [`stress_test_benchmark`](stress_test_benchmark.rs) example and the `stress_test` GitHub Actions workflow. |

## Running

From the repository root:

```bash
cargo run --release -- examples/hello.tape -o /tmp/hello.gif
```

Or build all of them at once:

```bash
for tape in examples/*.tape; do
    name=$(basename "$tape" .tape)
    cargo run --release -- "$tape" -o "/tmp/$name.gif"
done
```

The generated GIFs for every example are also published as a GitHub
release named `assets` and are linked from the project [README](../README.md).

## Adding a new example

1. Drop a new `name.tape` file into this folder.
2. Make sure its first `Output` directive uses a bare filename (no
    directory prefix) – the CI release workflow rewrites it to point at
    the upload directory.
3. Add a row to the table above.

## Stress-test benchmark

The [`stress_test_benchmark`](stress_test_benchmark.rs) example drives
[`stress_test.tape`](stress_test.tape) end-to-end through the evp library and
prints a one-page report with pipeline-health counters
(dropped raw-frame consumer frames, max queue depths, wall-clock time). It
exits non-zero when more than **5 %** of raw-frame consumer sends were dropped.

Run locally on a single physical core and compare against VHS in Docker:

```bash
cargo build --release --example stress_test_benchmark

# Pin to logical CPU 0 so the renderer/runner threads share one core –
# this is what the GitHub Actions workflow does too.
taskset -c 0 ./target/release/examples/stress_test_benchmark \
    /tmp/evp-stress_test.gif /tmp/evp-stress_test.report.txt

# Same scenario through VHS (single-core via docker):
install -D -m 0755 scripts/stress_test_program.py /tmp/stress_test_program.py
docker run --rm --cpus=1 --cpuset-cpus=0 \
    -v "$PWD/examples:/vhs" -v /tmp:/tmp \
    ghcr.io/charmbracelet/vhs:latest stress_test.tape

./scripts/stress_test_compare.sh \
    /tmp/evp-stress_test.gif /tmp/evp-stress_test.report.txt \
    examples/stress_test.gif /dev/null \
    /tmp/stress_test-comparison.md
```

The [`stress_test` workflow](../.github/workflows/stress_test.yml) runs both
sides automatically and uploads the comparison + both GIFs as a job
artifact.

### Analyzing any GIF's frame timestamps

[`scripts/gif_frame_analyzer.py`](../scripts/gif_frame_analyzer.py) is a
stdlib-only Python tool that walks a GIF's per-frame `delay` metadata
and reports how many frames look "long" given an expected framerate
(i.e. effectively skipped). It works on any animated GIF, not just the
stress_test output:

```bash
python3 scripts/gif_frame_analyzer.py path/to/anim.gif --fps 60
python3 scripts/gif_frame_analyzer.py path/to/anim.gif --fps 30 --json
```

The stress_test workflow also runs it on both renderers' GIFs and embeds
the results in `comparison.md`.
