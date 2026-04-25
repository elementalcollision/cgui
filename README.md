# cgui — visual front end for Apple's `container`

A fast, single-binary Rust TUI for [`apple/container`](https://github.com/apple/container)
built on [ratatui](https://ratatui.rs) + [crossterm](https://github.com/crossterm-rs/crossterm),
with a **Docker-compatible command shim** so muscle memory keeps working.

```
┌ cgui · Apple container front end ──────────────────────────────────────┐
│ [Containers]  Images  Volumes  Networks  Logs                          │
├──────────────────────┬─────────────────────────────────────────────────┤
│ CPU   12.4%          │ MEM    37.1% of limit                           │
│ ▁▂▃▅▇▆▅▃▂▁▁▂▃▅▇█▇▅▃ │ ▂▂▃▃▄▄▅▅▅▆▆▇▇▇▇▆▅▄▃▂                            │
├──────────────────────┴─────────────────────────────────────────────────┤
│ ID            IMAGE                            STATUS  CPUS  MEM       │
│▶ clerk-pg-test docker.io/pgvector/pgvector:pg16 running   4   1.0 GiB  │
│  redis-test    docker.io/library/redis:7        stopped   2  512 MiB  │
└────────────────────────────────────────────────────────────────────────┘
 q quit · ←→ tabs · ↑↓ select · r refresh · s start · x stop · K kill · d delete · l logs · a all
```

## Why

Apple's `container` is great but CLI-only. `cgui` gives you:

- **Live overview** — every running container, image, volume, and network at a glance, with sparklines for aggregate CPU/memory pulled from `container stats`.
- **One-key actions** — start, stop, kill, delete, view logs without leaving the TUI.
- **Drop-in `docker` muscle memory** — `cgui ps`, `cgui images`, `cgui rm`, `cgui rmi`, `cgui pull`, etc. translate to the right `container` invocation.

## Install

Requires:
- Rust 1.85+ (or update the pinned versions in `Cargo.toml`)
- Apple's `container` CLI on `$PATH` (and `container system start` running)

```bash
cargo build --release
cp target/release/cgui /usr/local/bin/   # optional
```

## Use

### TUI

```bash
cgui            # launch TUI
cgui tui        # same
```

| Key            | Action                                          |
| -------------- | ----------------------------------------------- |
| `q` / `Esc`    | Quit (or clear filter, if one is set)           |
| `Tab` / `→`    | Next tab                                        |
| `Shift+Tab`/`←`| Prev tab                                        |
| `↑` / `↓` / `j`| Move selection                                  |
| `Enter`        | **Inspect** — open scrollable JSON detail pane  |
| `/`            | **Filter** rows in current tab (Enter to apply) |
| `o`            | Cycle **sort** key for current tab              |
| `r`            | Refresh now                                     |
| `a`            | Toggle show-all vs running-only                 |
| `s`            | Start selected container                        |
| `x`            | Stop selected container                         |
| `K`            | Kill selected container (capital K)             |
| `d`            | Delete selected container                       |
| `l`            | Load logs for selected → Logs tab               |
| `e`            | **Exec** — drop into `/bin/sh` in selected container (Ctrl-D returns to TUI) |
| `p`            | **Pull** an image (Images tab) — opens prompt + live progress modal |

In the Detail pane: `↑↓`/`PgUp`/`PgDn` scroll, `Esc` closes.
In the Pull modal: `Esc` hides; pull keeps running in the background and the status bar reports completion.

State refreshes every 2s; sparklines smooth across ~4 minutes of history (120 samples).

### Docker-compat shim

| You type                | Runs                          |
| ----------------------- | ----------------------------- |
| `cgui ps [-a]`          | `container ls [-a]`           |
| `cgui images`           | `container image ls`          |
| `cgui rmi REF`          | `container image delete REF`  |
| `cgui pull REF`         | `container image pull REF`    |
| `cgui push REF`         | `container image push REF`    |
| `cgui tag SRC DST`      | `container image tag SRC DST` |
| `cgui login REGISTRY`   | `container registry login …`  |
| `cgui logout REGISTRY`  | `container registry logout …` |
| `cgui rm ID`            | `container delete ID`         |
| `cgui top`              | `container stats`             |
| `cgui run …`            | `container run …` (passthrough)|
| `cgui exec …`           | `container exec …` (passthrough)|
| `cgui logs …`           | `container logs …` (passthrough)|
| `cgui build …`          | `container build …` (passthrough)|

Anything not in the table is passed through unchanged, so the shim never gets in your way.

## Architecture

- `src/container.rs` — async wrapper around the `container` binary; always invokes `--format json` and decodes defensively into typed structs.
- `src/cli.rs` — `clap`-based Docker-compat verb translator.
- `src/app.rs` — pure TUI state machine (no I/O in render path).
- `src/ui.rs` — ratatui rendering: tabs, tables, sparklines, status bar.
- `src/main.rs` — terminal setup, input + tick loop on `tokio::select!`.

State refresh is async and best-effort: if one source (e.g. `volume ls`) fails, the rest still update and the error surfaces in the status bar.

## Roadmap

- ~~`exec` shell-out (drop the user into a pty for the selected container)~~ ✅
- ~~Image pull progress UI~~ ✅
- ~~Sort/filter columns~~ ✅
- ~~Detail pane on `Enter` (full `container inspect` JSON viewer)~~ ✅
- Optional GUI front end (Tauri) sharing the same `container.rs` core
- `inspect` syntax highlighting in the detail pane
- Multi-select for batch start/stop/delete
- `stats` per-row column overlay (live CPU/MEM in the Containers table)
