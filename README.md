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
| `Space`        | **Mark / unmark** the highlighted container for batch ops |
| `Enter`        | **Inspect** — open syntax-highlighted JSON detail pane |
| `/`            | **Filter** rows in current tab (Enter to apply) |
| `o`            | Cycle **sort** key for current tab              |
| `r`            | Refresh now                                     |
| `a`            | Toggle show-all vs running-only                 |
| `s`            | Start (operates on marked set, else highlighted row) |
| `x`            | Stop  (operates on marked set, else highlighted row) |
| `K`            | Kill  (operates on marked set, else highlighted row) |
| `d`            | Delete (operates on marked set, else highlighted row; clears marks on success) |
| `l`            | Load logs for selected → Logs tab               |
| `e`            | **Exec** — drop into `/bin/sh` in selected container (Ctrl-D returns to TUI) |
| `p`            | **Pull** an image (Images tab) — opens prompt + live progress modal |
| `b`            | **Build** an image (Images tab) — two-field prompt, then streaming modal |
| `P`            | **Re-attach** to a backgrounded pull or build      |
| `?`            | Toggle the **per-tab help** overlay                |
| **Mouse L**    | Click a tab title to switch tabs · click a row to select it |
| **Mouse R**    | Right-click anywhere → **context menu** of actions for the current tab |
| **Wheel**      | Scroll Logs · Inspect detail · Pull/Build stream   |
| `X`            | Open the **runtime profile picker** (switch which CLI cgui shells out to) |
| `Ctrl-R`       | (in Logs `/` search) toggle **regex** mode         |
| `Ctrl-O`       | (in Build prompt) open the **file picker** for the build context |
| `F`            | (Containers) start **follow-mode log streaming** · (Logs) toggle stop/start |
| `↑` / `↓`      | (in Pull/Build prompts) cycle through **recent presets** |
| `T`            | (Images) **Trivy scan** of selected image (HIGH+CRITICAL) |
| `u` / `D`      | (Stacks) **Up** / **Down** the selected stack       |
| `n` / `E`      | (Stacks) **New** stack (template + `$EDITOR`) · **Edit** selected stack |
| `1`-`4` / `0`  | (Trivy modal) filter by severity (CRITICAL/HIGH/MEDIUM/LOW) · clear |
| `/`            | (Trivy modal) **search bar** across CVE id / package / title |
| `L`            | (Stacks) **multi-follow** logs from every service (prefixed) |

On the Logs tab `/` enters **search-as-you-type**: matches highlight in yellow as you type, with a live match counter in the title (`Logs · foo · search:err  (4 matches)`). Enter exits the input but keeps the highlight; `Esc` clears.

The pull modal renders a colored **Gauge** driven by a permissive parser of the streamed output (recognises `42%`, `12.3MB/45.6MB`, and `3/8` layer ratios — newest match wins). `Esc` backgrounds the modal: a yellow `⟳ pulling ref 42% — P to view` chip appears in the status bar so you can re-open it any time. When the pull finishes the chip turns green; pressing `P` shows the final log.

The Containers table shows **live CPU% and MEM** (used / limit) per row when a stats sample is available, with traffic-light coloring. Marked rows display a yellow `●` in the leftmost column.

On the **Volumes tab**, `Enter` opens a richer detail pane: capacity from the CLI, actual on-disk size from the backing image (sparse images are honest about it), a unicode fill bar (`[████░░░░░░] 42.3%`), and the full inspect JSON below.

User preferences (last tab, per-tab sort key, show-all toggle) are persisted to `$XDG_CONFIG_HOME/cgui/state.json` (defaults to `~/.config/cgui/state.json`). Saved on every relevant change and on quit; missing or malformed files are silently ignored.

### Theme

Drop a `theme.toml` next to the state file:

```toml
# ~/.config/cgui/theme.toml — all fields optional
accent  = "#88c0d0"   # tab highlight, modal borders, headers
primary = "white"     # default body text
muted   = "darkgray"  # punctuation, hints, dim labels
success = "#a3be8c"   # running status, ok results
warning = "yellow"    # marks, mid-progress, in-flight
danger  = "red"       # stopped, errors, high CPU
info    = "blue"      # image refs, links
```

Accepts named colors (`red`, `darkgray`, `lightcyan`, …), `#RRGGBB`, and `rgb(r, g, b)` for truecolor terminals. Missing fields fall back to the built-in defaults; a malformed file is silently ignored.

### Runtime profiles

cgui can drive any Docker-compatible CLI, not just Apple's `container`. Drop a `profiles.toml` next to `state.json`:

```toml
# ~/.config/cgui/profiles.toml
default = "container"

[[profile]]
name = "container"
binary = "container"

[[profile]]
name = "docker"
binary = "/usr/local/bin/docker"

[[profile]]
name = "podman"
binary = "/opt/homebrew/bin/podman"
```

Press `X` in the TUI to open the picker, ↑↓ + Enter to activate. The choice is saved to `state.json` so `cgui ps`, `cgui images`, etc. (the Docker-compat shim) honor it on next launch too. The active runtime is shown in the top header (`cgui · runtime: docker`).

### Resource alerts

The `[alerts]` section of `theme.toml` configures per-row CPU/MEM thresholds:

```toml
[alerts]
cpu_warn  = 60.0   # tint the row when CPU% exceeds this (steady)
cpu_alert = 85.0   # pulse when CPU% exceeds this
mem_warn  = 70.0
mem_alert = 90.0
pulse     = true   # set to false for steady highlight at alert level
```

The Containers row's background is steady-tinted at `warn` and pulses at `alert` (alternating once per ~500 ms). Defaults are 60/85/70/90 with pulse on.

### Recent presets

The pull and build prompts remember your last 10 invocations. `↑` cycles into the history (saving whatever you'd typed), `↓` cycles back; the prompt footer shows your position (e.g. `↑↓ recent (2/7)`). Storage is in the same `state.json` next to the rest of your prefs.

### Follow-mode logs

Press `F` on a Containers row to start a `container logs -f` stream into the Logs tab; press `F` again on the Logs tab to stop. The header colors green and shows `● follow` while live; auto-tails when scroll is at the top, otherwise pins to your scroll position. Combined with `/` + `Ctrl-R`, you get live regex log monitoring.

### Stacks

The **Stacks** tab is a tiny compose-style runner. Each stack lives in `~/.config/cgui/stacks/<name>.toml`:

```toml
name = "myapp"

[[service]]
name = "db"
image = "docker.io/pgvector/pgvector:pg16"
env = { POSTGRES_USER = "test", POSTGRES_PASSWORD = "test" }
ports = ["15432:5432"]
volumes = ["dbdata:/var/lib/postgresql/data"]

[[service]]
name = "api"
image = "myapp/api:latest"
network = "default"
depends_on = ["db"]
ports = ["8080:8080"]
```

In the TUI: `u` brings the stack up (`container run -d --name <stack>_<service> …` per service in topological dependency order), `D` tears it down (stop + delete every service container, in reverse). Both stream into the same modal as pull/build, so you see exactly what's executing. The `RUNNING` column shows `<up>/<total>` per stack with traffic-light coloring.

A starter `example.toml` is dropped on first run. The Stacks tab's `Enter` opens a detail pane showing the parsed services and the exact `container run` plan.

#### Templates and `cgui new`

```bash
cgui templates                         # list built-in templates
cgui new myapp --template postgres+api # scaffold ~/.config/cgui/stacks/myapp.toml
cgui new cache  --template redis
cgui new web    --template nginx
cgui new minimal                       # default: blank
```

Built-in templates: `blank`, `postgres`, `postgres+api`, `redis`, `nginx`. Each is a hand-tuned starting point with sane healthchecks, restart policy, and per-stack volume names interpolated from the stack name. Editing afterwards is expected — templates aren't a generator, just a head start. Errors out on collision so you can't clobber an existing stack.

#### Live diff (`=`)

Press `=` on a Stacks row to compute a **live diff** between the TOML on disk and the actual running containers. cgui calls `container inspect <stack>_<service>` for each service and compares: image reference, published ports (sorted), per-key env, attached network, plus container status. The modal shows:

- `✓ field          value` when TOML matches runtime
- `⚠ field          expected X / actual Y` when they drift
- `✗ missing        no container — u to bring up` when the service has never been run
- `! status         exited` when the container exists but isn't running

Title shows a `<matched>/<total>` counter; the border colors green when everything matches, yellow otherwise. Useful for catching "I edited the TOML but forgot to `D` then `u`" drift, or seeing which env vars from the running container differ from your declarations.

#### Restart policy + healthchecks

Each service can declare a `restart` policy and a `healthcheck` block:

```toml
[[service]]
name = "db"
image = "docker.io/pgvector/pgvector:pg16"
ports = ["15432:5432"]
restart = "always"          # "always" | "on-failure" | "no" (default)

[service.healthcheck]
kind = "tcp"                # "tcp", "http", or "cmd"
target = "15432"            # tcp: host port · http: PORT/PATH or full URL
interval_s = 30             # default 30
# command = ["pg_isready", "-U", "postgres"]   # for kind = "cmd"
# expect_status = [200, 299]                    # http only; default 200..399
```

For `kind = "http"` the `target` accepts:
- a bare port (`"8080"`) — probes `http://127.0.0.1:8080/`
- `PORT/PATH` (`"8080/healthz"`) — probes `http://127.0.0.1:8080/healthz`
- a full HTTP URL (`"http://example.com:8080/v1/ping"`) — hand-rolled HTTP/1.0 client over `tokio::TcpStream`, no extra deps
- a full **HTTPS URL** (`"https://example.com/v1/ping"`) — shells out to `curl --silent --max-time 2 -o /dev/null -w "%{http_code}"` so we get TLS without pulling in `rustls`/`native-tls`. macOS `curl` covers it.

Success is any status in `expect_status[0]..=expect_status[1]`, defaulting to `200..399`.

A background loop (every ~10 s) checks each service's container state. If the policy is `always`, any stopped/exited container is restarted; `on-failure` only restarts on a non-zero exit. The `HEALTH` and `RESTART` columns on the Stacks tab show the rolled-up state per stack:

- `HEALTH`: `✓ healthy (N)` / `✗ unhealthy` / `partial` / `waiting` / blank if no service has a healthcheck
- `RESTART`: e.g. `always:2 on-fail:1` / `—` if no service has a policy

The detail pane (Enter on a stack) shows per-service health probe results with the last message.

#### Live reload

Stack files are watched via FSEvents on macOS. Editing a `*.toml` in `~/.config/cgui/stacks/` triggers an automatic reload — no need to press `r` or restart the TUI. New files appear immediately; deleted files vanish on the next refresh tick.

### Update detection

cgui checks for newer releases of Apple's `container` runtime and of cgui itself, once per startup, against the public GitHub Releases API. Results are cached in `state.json` for 24 hours.

When something is behind, an `⬆ container 0.12.3 → 0.13.0 · U to view` chip appears in the status bar (one per component). Press `U` to open the **update prompt** — a centered modal showing:

- component, installed → latest, published date, release URL
- the first ~80 lines of the GitHub release notes (scrollable with ↑↓/PgUp/PgDn)

Inside the modal:

- `O` opens the release URL in your default browser (`open <url>` on macOS)
- `D` **downloads** the signed installer `.pkg` to `~/Library/Caches/cgui/` — see below
- `L` dismisses *that component* for the rest of the session (the chip vanishes; comes back next launch)
- `←` / `→` (or `n` / `p`) cycle if multiple components are behind
- `Esc` closes the modal

#### `[D]ownload`

Spawns `curl -fL --silent` for the release's `*-installer-signed.pkg` asset (the **signed** variant only — cgui deliberately refuses the unsigned `.pkg` so the install path stays safe by default). Writes to `~/Library/Caches/cgui/<asset-name>` via a `.part` tempfile that's atomically renamed on success; partial downloads are removed on any failure or size mismatch.

Progress streams into the same modal we use for pull/build/scan: a "Downloading container 0.13.0…" header plus a once-a-second `12.4 MiB / 68.0 MiB (18.2%)` line. Cache reuse is automatic — re-running `[D]` on a release whose `.pkg` is already in the cache (and matches the API-reported size exactly) skips the download and reports `✓ cached at <path>` immediately.

Phase 4 will use the cached path to invoke `sudo installer -pkg <path> -target /`. Phase 3 stops at "downloaded" — nothing is run with elevated privileges.

#### `[I]nstall`

Pressing `I` in the update modal kicks off the install path. cgui auto-detects how the runtime was installed and picks the right route:

- **`.pkg` install** (the default — runtime under `/usr/local/`): cgui chains `[D]` first if the asset isn't cached, then **suspends the TUI** (leaves alt-screen + raw mode) and runs `sudo installer -pkg <cached-pkg> -target /`. The `sudo` password prompt and installer's progress land on your real terminal — cgui doesn't intercept either. The TUI is restored on exit.
- **Homebrew install** (binary under `/opt/homebrew/Cellar/` or `/usr/local/Cellar/`): no download, no sudo. cgui suspends the TUI and runs `brew upgrade container` so brew's chatty output is visible.

Either path is followed by a **post-install verify**: cgui re-runs `container --version`, parses it, and compares to the expected `latest`. On a confirmed upgrade the chip vanishes immediately and the status bar reads `✓ upgraded container to 0.13.0`. On version mismatch (`installer` claimed success but the version didn't change) you get `⚠ installer ran but container is X (expected Y)` so you can investigate.

Failure modes are explicit: download failed → `install cancelled (download failed)`; installer non-zero exit → `installer exited <status>`; sudo cancelled → no version change reported. The cached `.pkg` is left in place so you can retry without re-downloading.

#### cgui self-update

cgui can also upgrade itself. The route depends on how you installed it (auto-detected from `current_exe()`):

- **Standalone binary** (the default): `[I]` downloads the matching release asset (preferring `cgui-<arch>-apple-<os>` archives, falling back to a bare `cgui` binary) and **atomic-replaces** the running binary via `std::fs::rename`. POSIX guarantees this is safe even on a running process — the kernel keeps the old inode mapped while cgui keeps running. After the rename, the status bar reads `✓ replaced cgui binary — restart to use 0.13.0`. No sudo, no terminal teardown.
- **Homebrew install** (binary under `/opt/homebrew/Cellar/`): `brew upgrade cgui`, same suspended-TUI pattern as the runtime brew path.
- **Cargo install** (binary under `~/.cargo/bin/`): cgui won't muck with cargo state. The status bar surfaces `cargo-installed cgui — upgrade with cargo install cgui --force` and lets you run it yourself.

Tarballs (`*.tar.gz` / `*.tgz` / `*.tar`) are extracted via the system `tar` binary and the `cgui` file is located inside (any depth). The new binary is staged at `<current_exe>.new` with mode 0755, then renamed over the running file. Failures clean up the `.new` tempfile and the extraction tmp dir; the running process is untouched.

`cgui doctor` includes the same information without launching the TUI; `cgui update` forces a fresh API hit (bypasses the 24h cache and the opt-out).

Disable entirely with `cgui --no-update` (persists `auto_update_check = false` in `state.json`). The opt-out is honoured by the background check and `cgui doctor`; the explicit `cgui update` subcommand always runs.

Network: macOS's built-in `curl` is used so no extra dependency is added; calls are bounded by an 8-second timeout and skipped silently on failure.

### `cgui doctor`

```
$ cgui doctor
== cgui doctor ==
✓ active profile: container → container
✓ `container` resolves to /usr/local/bin/container
✓ `container --version` → container CLI version 0.11.0
✓ container system status: running
! no profiles.toml at ~/.config/cgui/profiles.toml (using built-in default)
✓ state.json at ~/.config/cgui/state.json parses cleanly
! trivy not on PATH (image scan disabled — `brew install trivy`)
== all checks passed ==
```

Exit code 0 if everything's green, 1 otherwise. Useful for CI or scripting.

### Trivy image scan

If [trivy](https://github.com/aquasecurity/trivy) is on `$PATH`, press `T` on an Images row (or right-click → Trivy scan). Runs `trivy image --format json --severity HIGH,CRITICAL <ref>`, streams stderr progress into the standard op modal, then on completion **switches to a parsed results modal** with:

- Severity-colored count chips at the top (`CRITICAL 3 · HIGH 12 · …`)
- A scrollable table of findings (sev / CVE / package / installed / fixed / title), sorted critical-first
- ↑↓/PgUp/PgDn to scroll, Esc to close

If parsing fails (older trivy schema or malformed output) the raw stream stays visible — no data is lost.

In the results modal: press `1`/`c` for CRITICAL, `2`/`h` for HIGH, `3`/`m` for MEDIUM, `4`/`l` for LOW, or `0` to clear the filter. The active severity gets an underline on its count chip. Press `/` to enter the **CVE / package / title search bar**: substring filter applied on top of the severity filter, case-insensitive across the CVE id, package name, and title fields. Enter exits the input keeping the filter; Esc clears it.

### Importing docker-compose.yml

```bash
$ cgui import-compose ./docker-compose.yml --name myapp
$ cgui import-compose ./docker-compose.yml --name myapp --write   # writes to ~/.config/cgui/stacks/myapp.toml
```

Translates a useful subset of compose v2/v3 (`image`, `environment` map+list, `ports`, `volumes`, `depends_on` list+map, `networks`, `command` string+list) into a cgui stack TOML body. Unknown keys are silently dropped — this is a pragmatic translator, not a full compose engine. Without `--write` the result goes to stdout so you can pipe or eyeball it.

### Editing stacks in-TUI

On the Stacks tab:
- `n` → name prompt → writes a template file → opens in `$EDITOR` (defaults to `vi`)
- `E` → opens the highlighted stack's source file in `$EDITOR`

The TUI is fully suspended while the editor runs (alt-screen left, raw mode off) and rebuilt cleanly on exit. The stack list reloads from disk afterwards so your edits show up immediately.

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

## Progress

| Feature                                              | Status     | Landed in       |
| ---------------------------------------------------- | ---------- | --------------- |
| Tabs · Containers/Images/Volumes/Networks/Logs       | ✅ shipped | 0.1.0           |
| Aggregate CPU/MEM sparklines                         | ✅ shipped | 0.1.0           |
| One-key lifecycle (start/stop/kill/delete/logs)      | ✅ shipped | 0.1.0           |
| Docker-compat CLI shim (`ps`, `images`, `rmi`, …)    | ✅ shipped | 0.1.0           |
| `e` exec shell-out (`/bin/sh` in selected container) | ✅ shipped | 0.2.0           |
| `p` image pull with live streaming progress modal    | ✅ shipped | 0.2.0           |
| `/` filter + `o` sort across all resource tabs       | ✅ shipped | 0.2.0           |
| `Enter` inspect detail pane (`container inspect` JSON)| ✅ shipped | 0.2.0           |
| Per-row live CPU/MEM in Containers table             | ✅ shipped | 0.3.0           |
| `Space` multi-select + batch start/stop/kill/delete  | ✅ shipped | 0.3.0           |
| Syntax-highlighted JSON in inspect pane              | ✅ shipped | 0.3.0           |
| Parsed % gauge for image pulls                       | ✅ shipped | 0.4.0           |
| Search-as-you-type in Logs tab (highlighted matches) | ✅ shipped | 0.4.0           |
| `Esc` backgrounds pull modal · `P` re-attaches       | ✅ shipped | 0.4.0           |
| Volume detail: capacity + on-disk usage + fill gauge | ✅ shipped | 0.5.0           |
| Per-tab help overlay (`?`)                           | ✅ shipped | 0.5.0           |
| Mouse: click tabs and rows to select                 | ✅ shipped | 0.5.0           |
| Persisted prefs (tab, sort, show-all) at `~/.config/cgui/state.json` | ✅ shipped | 0.5.0 |
| Wheel scroll in long views (logs, inspect, op stream) | ✅ shipped | 0.6.0          |
| Right-click context menu                              | ✅ shipped | 0.6.0          |
| Configurable theme via `~/.config/cgui/theme.toml`    | ✅ shipped | 0.6.0          |
| `b` image build with same streaming progress modal    | ✅ shipped | 0.6.0          |
| Per-container CPU sparkline column                    | ✅ shipped | 0.7.0          |
| Regex log search (`Ctrl-R` toggles in `/`)            | ✅ shipped | 0.7.0          |
| Build context file picker (`Ctrl-O` from build prompt)| ✅ shipped | 0.7.0          |
| Runtime profile switcher (`X`) + `profiles.toml`      | ✅ shipped | 0.7.0          |
| Recent pull/build presets (↑↓ in prompts)             | ✅ shipped | 0.8.0          |
| Follow-mode log streaming (`F`) with auto-tail        | ✅ shipped | 0.8.0          |
| Configurable resource alerts (`[alerts]` in theme)    | ✅ shipped | 0.8.0          |
| `cgui doctor` environment health check                | ✅ shipped | 0.9.0          |
| Network detail pane (mode/state/subnets/nameservers)  | ✅ shipped | 0.9.0          |
| Trivy image scan (`T` on Images tab)                  | ✅ shipped | 0.9.0          |
| **Stacks** tab — compose-style multi-service sessions | ✅ shipped | 0.9.0          |
| Stack create/edit in TUI (`n`/`E` → `$EDITOR`)        | ✅ shipped | 0.10.0         |
| Parsed Trivy results modal (severity-grouped table)   | ✅ shipped | 0.10.0         |
| `cgui import-compose` (docker-compose.yml → stack)    | ✅ shipped | 0.10.0         |
| Stack `restart` + `[service.healthcheck]` (tcp/cmd)   | ✅ shipped | 0.11.0         |
| Trivy filter-by-severity (1-4 / c/h/m/l, 0 clears)    | ✅ shipped | 0.11.0         |
| FSEvents-driven stack-file live reload                | ✅ shipped | 0.11.0         |
| MIT LICENSE                                           | ✅ shipped | 0.11.0         |
| Decoupled refresh + 8s timeouts on CLI calls          | ✅ shipped | 0.11.1         |
| HTTP healthcheck kind                                 | ✅ shipped | 0.12.0         |
| Per-service log multiplex (`L` on Stacks)             | ✅ shipped | 0.12.0         |
| Trivy CVE search bar (`/` in results modal)           | ✅ shipped | 0.12.0         |
| Update detection (status chip + `cgui doctor` row)    | ✅ shipped | 0.13.0         |
| Update prompt modal (`U`, `[O]pen`, `[L]ater`)        | ✅ shipped | 0.13.1         |
| Update download (`[D]`) to `~/Library/Caches/cgui/`   | ✅ shipped | 0.13.2         |
| Update install (`[I]`) — sudo installer / brew + verify | ✅ shipped | 0.13.3       |
| cgui self-update — atomic-replace / brew / cargo hint | ✅ shipped | 0.13.4         |
| Stack templates + `cgui new`                          | ✅ shipped | 0.14.0         |
| HTTPS healthcheck (`https://…` target via `curl`)     | ✅ shipped | 0.14.0         |
| Live stack diff (`=` on Stacks tab)                   | ✅ shipped | 0.14.0         |
| Optional GUI front end (Tauri)                        | 🟡 planned | —              |

## Roadmap

- Optional GUI front end (Tauri) sharing the same `container.rs` core
- HTTPS healthcheck (currently plain HTTP only)
- Live diff view between stack file on disk and running containers
- Stack templates / presets (`cgui new myapp --template postgres+api`)

## License

[MIT](LICENSE) © 2026 Dave Graham
