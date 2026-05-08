# evp examples

This directory contains small `.tape` scripts that double as smoke tests
for the evp recorder, the GIF renderer and (eventually) the animated SVG
backend.

| Script | What it shows |
| --- | --- |
| [hello.tape](hello.tape) | Bare-minimum recording – type a single command. |
| [shell-tour.tape](shell-tour.tape) | Multiple commands paced with `Wait` instead of `Sleep`. |
| [keys.tape](keys.tape) | Modifier keys + line-editing (`Ctrl+U`). |
| [colors.tape](colors.tape) | ANSI SGR colour table – stresses the cell encoder. |
| [progress.tape](progress.tape) | In-place line rewrites – stresses the diff path. |

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
