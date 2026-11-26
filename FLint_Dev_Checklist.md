# FLint — Development Checklist (v2 / Direction C)
### DAW-Aware, Pure-P2P Collaboration for FL Studio (Rust + Iroh + JUCE VST3)

> See [ARCHITECTURE.md](./ARCHITECTURE.md) for the full system design, data model, IPC contract, security model (§7) and disk-usage model (§8).
>
> **Current version:** daemon `0.2.1` · **Last updated:** 2026-11-26 · **Next milestone:** M3 (daemon server)

---

## How to Use This Checklist
- Build in **vertical slices**: each phase produces something real and testable on its own.
- Every phase ends with a **Done when** line. Don't start the next phase until it holds.
- **Phases 1–3** build the Rust daemon. You can test `.flp` diffing, missing-plugin checks and P2P sync from the CLI before touching C++.
- **Phases 4–5** build the JUCE VST3 plugin and connect it to the daemon. With Phase 8 (real-world testing) and Phase 9 (packaging), this is **V1.0**.
- **Phases 6–7** (bar-locked comments, stem crate) are **V1.1**. They're deliberately after the first release so FLint ships with a small, solid core.
- Status markers: `[x]` done · `[ ]` to do · `[~]` done, but needs checking on real FL Studio projects.

---

## Release Plan

| Milestone | Scope | Exit criteria | Status |
|---|---|---|---|
| **M1** — Project intelligence | `.flp` parser, musical diff, plugin scanner | `inspect` / `diff` / `check` correct on real projects | ✅ Done (real-project checks pending, see 1.4) |
| **M2** — P2P engine | Iroh sync, dedup, multi-writer, seeder, prune, rotate | Two-peer tests pass offline + over internet | ✅ Done in code; physical two-machine test pending |
| **M3** — Daemon server | Background service, localhost API, file watcher, presence | Plugin-free end-to-end flow works through HTTP/WS only | ⏳ Next |
| **M4** — Plugin MVP | Plugin connected to daemon: timeline, push/pull, diffs, warnings | Full push → pull loop from inside FL Studio, no UI stutter | ⬜ |
| **M5** — Audio previews | Bar-synced capture + in-plugin audition | Every push can carry a playable preview | ⬜ |
| **M8 + M9** — **V1.0** | Real-world testing, installer, CI, docs | A non-developer collaborator installs and syncs without help | ⬜ |
| **M6 + M7** — V1.1 | Bar-locked comments, stem fallback, shared crate | Collaborators exchange notes and stems without leaving FL | ⬜ |

---

## Phase 0 — Environment & Workspace Setup

### 0.1 — Rust Workspace (`daemon/`)
- [x] Install Rust toolchain (`rustup`, stable `cargo`) and verify `cargo --version`
- [x] Create Cargo workspace at `daemon/Cargo.toml` with member crates:
  - `crates/flint-core` (shared types: `ProjectManifest`, `MusicalDiff`, `Snapshot`, `Comment`, `CrateItem`)
  - `crates/flint-flp` (`.flp` binary parser & diff engine)
  - `crates/flint-vst-scan` (installed plugin scanner)
  - `crates/flint-sync` (`iroh` P2P layer)
  - `crates/flint-daemon` (HTTP/WS daemon + CLI + `seeder` mode)
- [x] Verify `cargo check --workspace` succeeds cleanly
- [x] Pin `iroh` / `iroh-blobs` / `iroh-docs` / `iroh-gossip` to exact versions; upgrade all four together (see comment in `daemon/Cargo.toml`) *(v0.2.1)*

### 0.2 — Migrate JUCE Plugin to CMake (`plugin/`)
- [x] Verify CMake 3.22+, Visual Studio C++ workload, and JUCE path (or fetch JUCE via `FetchContent` for reproducible builds)
- [x] Add `plugin/CMakeLists.txt` targeting `VST3` + `Standalone` (for rapid UI testing outside FL Studio)
- [x] Confirm `FLint.vst3` builds cleanly via CMake in both Debug and Release
- [x] Add `daemon/target/` and CMake `build/` directories to `.gitignore`
- [ ] Move/archive legacy `sidecar/` Node prototype (Phase 2 now passes, so this can be done any time; e.g. move it to an `archive/v1-sidecar` tag and delete the folder)

### 0.3 — Developer Machine Notes
- [x] **Keep build output out of OneDrive.** Set `CARGO_TARGET_DIR` (and the CMake build dir) to a folder outside OneDrive. Otherwise OneDrive syncs gigabytes of build output and builds slow down dramatically.
- [x] **Machines without Visual Studio C++ (no admin):** Rust works with the `x86_64-pc-windows-gnu` toolchain plus a portable MinGW-w64 GCC. Debug builds must optimise dependencies, because `iroh` also builds as a `cdylib` and the GNU linker otherwise fails with *export ordinal too large*:
  `cargo test --workspace --config 'profile.dev.package."*".opt-level=2'`
  The MSVC toolchain does not need this.
- [ ] Add a `daemon/README.md` "Building" section covering both toolchains

---

## Phase 1 — `flint-flp`: `.flp` Parser, Musical Diffs & Plugin Scanner (M1)

> **Goal:** Run a CLI command against two real `.flp` files and get an accurate musical diff + missing-plugin report.
>
> **Done when:** `inspect`, `diff` and `check` give correct output for at least 5 real projects of different sizes and FL versions.

### 1.1 — `.flp` Binary Parser (`flint-flp`)
- [x] Parse `FLhd` header (`format`, `num_channels`, `ppq`) and validate magic bytes
- [x] Parse `FLdt` chunk and implement the 4-band TLV event reader (`0..=63` byte, `64..=127` word, `128..=191` dword, `192..=255` varint length-prefixed data)
- [x] Extract core project metadata: FL Studio version string, BPM (including fine tempo), time signature, title
- [x] Extract **Channels**: index, name, generator/plugin name, sample file path (resolving `%FLStudioUserData%` / relative paths where applicable)
- [x] Extract **Mixer Inserts**: index, name, FX slot plugin names (handling Fruity Wrapper VST2/VST3 names)
- [x] Extract **Patterns**: index, name, note count + deterministic content hash of pattern notes
- [x] Support reading `.flp` directly *or* from inside a Zipped Loop `.zip` without manual extraction
- [x] Write unit tests and verify against real `.flp` / `.zip` projects *(verified on `first melodic techno.flp` (FL 24.2.2) and `im on team cece.zip` (macOS FL 26.1.5 with Serum 2 + ShaperBox 3))*
- [~] Extract **Arrangements**: name + hash/size of playlist clip data (events 99 / 241 / 233 per PyFLP) *(v0.2.1; synthetic tests only)*
- [~] Hash each channel's and mixer FX slot's saved plugin state, to detect "settings changed" *(v0.2.1)*
- [~] Find sample paths embedded in native plugin state (Slicex, DirectWave, FPC, …); third-party VSTs are skipped on purpose *(v0.2.1)*
- [x] `rewrite_sample_paths()`: rewrite channel sample paths (event 196) without touching anything else; a no-op rewrite is byte-identical *(v0.2.1)*

### 1.2 — Semantic Diff Engine (`flint-flp`)
- [x] Implement `diff_manifests(old, new) -> MusicalDiff`
- [x] Detect and format human-readable summaries for:
  - Tempo / FL version changes
  - Added / removed / renamed channels and plugins
  - Added / removed / reordered FX on mixer inserts
  - Added / removed / modified patterns (including note count deltas)
  - Added / removed audio sample files
- [x] CLI smoke test: `flint-daemon inspect project_v1.flp` and `flint-daemon diff project_v1.flp project_v2.flp`
- [x] Match channels and patterns by content → unique name → index, so inserting one item doesn't make everything after it look changed; moves are summarised in one line *(v0.2.1)*
- [~] Report plugin settings changes and arrangement edits (clips added / removed / moved) *(v0.2.1)*

### 1.3 — Installed Plugin Scanner (`flint-vst-scan`)
- [x] Scan Windows VST3 (`C:\Program Files\Common Files\VST3`), common VST2 paths, and configurable custom folders
- [x] Normalize plugin names (strip `.vst3`/`.dll`, handle architecture suffixes like `(x64)`) + maintain FL Studio stock plugin allowlist so native FL plugins never trigger false-positive warnings
- [x] Implement `check_compatibility(manifest, peer_env) -> CompatibilityReport` listing missing VSTs with their exact Channel / Mixer Insert location and FL version mismatch warnings
- [x] Scan per-user folders (`%LOCALAPPDATA%\Programs\Common\VST3` / `CLAP`) and the `FLINT_PLUGIN_DIRS` env var *(v0.2.1)*
- [x] Read FL Studio's own plugin database (`Documents\Image-Line\…\Plugin database\Installed`) for plugin names exactly as FL shows them *(v0.2.1)*
- [ ] Fill `PeerEnvironment.fl_version` (currently always `None`): read the installed FL Studio version from the registry (`HKCU\Software\Image-Line\…`) or the newest `.flp` the user saved

### 1.4 — Real-Project Verification *(do this first on your personal laptop)*
- [ ] Build a **fixture corpus** under `daemon/crates/flint-flp/tests/fixtures/`: 5–10 small real projects (different FL versions, one with Fruity Wrapper VSTs, one with Slicex/FPC, one with multiple arrangements). Strip or replace private samples. Add a test that parses every fixture without error.
- [ ] `inspect` each fixture: confirm arrangements, channels, mixer FX and samples look right
- [ ] **Noise check:** save a project twice with no edits and `diff` the two copies. Any "settings changed" line means that plugin saves view state; add it to an ignore-list (or hash only its real state sub-chunk)
- [ ] Make one edit of each kind (move a clip, tweak a knob, add a note, insert a channel) and confirm each produces exactly one sensible diff line
- [ ] **Fuzz the parser** (`cargo fuzz` on `parse_flp_bytes` and `rewrite_sample_paths`): a corrupt `.flp` must return an error, never panic or hang

---

## Phase 2 — `flint-sync`: Pure-P2P Multi-Writer Engine with Iroh (M2)

> **Goal:** Two machines on different networks exchange deduplicated `.flp` snapshots, samples, comments, and crate items via pure P2P, with multi-writer support and an optional always-on seeder.
>
> **Done when:** the physical two-machine test in 2.5 passes on a real project.

### 2.1 — Iroh Core + Blobs (`iroh` + `iroh-blobs`)
- [x] Set up persistent `iroh::Endpoint` with stored Ed25519 secret key in `%LOCALAPPDATA%\FLint\`
- [x] Initialize persistent `iroh-blobs` store (BLAKE3 content-addressed storage)
- [x] Implement project ingest: hashes `.flp` and each referenced sample individually into `iroh-blobs` (no zipping, automatic deduplication)
- [x] Stream files from disk when hashing/adding; never load whole samples into memory *(v0.2.1)*
- [x] Keep zip folder structure (`Drums/kick.wav` and `Bass/kick.wav` both survive) *(v0.2.1)*
- [x] Report samples missing on the pusher's disk (`missing_samples`) instead of silently skipping them *(v0.2.1)*
- [x] Pull = download missing files on demand from the author / collaborators / seeders, with retry until `--wait-secs` *(v0.2.1)*
- [x] Pull writes to a **new** folder every time (`<id>`, `<id>-2`, …) and rewrites sample paths in the pulled `.flp` (original kept in `.flint/original.flp`) *(v0.2.1)*
- [x] `session sync [--all]` and prefetch-on-join to download files ahead of time *(v0.2.1)*
- [x] Blob garbage collection (protected by FLint tags + iroh-docs entries) and `session prune --keep N` *(v0.2.1)*

### 2.2 — Multi-Writer Session Sync (`iroh-docs` + `iroh-gossip`)
- [x] Implement `create_session()` → creates an `iroh-docs` namespace and returns a shareable invite ticket
- [x] Implement `join_session(ticket)` → joins namespace and starts document sync
- [x] Publish local environment (`peers/<node_id>/env`) on join and startup
- [x] Implement multi-writer push writing `snapshots/<ulid>_<author>` with `parent_ids`, `manifest`, `diff_summary`, `flp_blob`, `assets`, and optional `preview_audio_blob`
- [x] Track each folder's base version in `.flint/<session_id>.json`; parent = `--parent` → folder record → newest; diff against the real parent; flag `diverged_from_latest` *(v0.2.1)*
- [x] Implement `upsert_comment()` (`comments/<snapshot_id>/<ulid>`) and `add_crate_item()` (`crate/<ulid>`)
- [x] Saved display name: `flint-daemon profile set --name` *(v0.2.1)*
- [ ] **Live peer presence**: corrected, this was marked done before but is *not implemented*. `subscribe_session()` exposes raw `LiveEvent::NeighborUp/Down`; nothing turns them into "Alex is online". Moved to Phase 3.4.

### 2.3 — Optional Always-On Seeder Mode
- [x] Add `flint-daemon seeder` headless mode (`--data-dir <path>`)
- [x] `seeder --add <ticket>` pins a session; write tickets are downgraded to read-only unless `--allow-write` *(v0.2.1)*
- [x] Seeder prefetches every snapshot's files on a loop (`--prefetch-interval-secs`) *(v0.2.1)*
- [ ] Document a 5-minute VPS / Raspberry Pi setup (systemd unit file + firewall notes) in the README
- [ ] Run the seeder as a background service on Windows too (per-user scheduled task; no admin)

### 2.4 — Access Control
- [x] `session share --read-only` / `--hide-ip` *(v0.2.1)*
- [x] `session rotate` to remove a collaborator (history copied to a new namespace; old one left) *(v0.2.1)*
- [ ] **Client-side encryption** so seeders store files they cannot read: per-session symmetric key shared only in write/collaborator tickets, encrypt blob contents + snapshot JSON before ingest. Needs a design note first (key rotation interacts with `session rotate`; dedup only works within a session).
- [ ] Restrict `iroh-blobs` serving to session members (currently anyone who knows a hash can fetch it). Look at `BlobsProtocol` event hooks for an allow-list of endpoint IDs from `peers/*`.

### 2.5 — Physical Two-Machine Test
- [x] Automated two-peer tests: offline (`two_peer_sync_local`) and over the internet (`two_peer_sync_over_internet`, run with `--ignored`) *(v0.2.1)*
- [ ] Two real machines on **different networks** (e.g. home Wi-Fi + phone hotspot): push a real project with samples from A, pull on B, open in FL Studio, no missing-sample dialog
- [ ] Edit on B and push; confirm A sees B's version with the right parent and a sensible diff
- [ ] Push #2 after a one-note edit: confirm only the `.flp` transfers
- [ ] A pushes, A goes offline, B pulls via the seeder

---

## Phase 3 — `flint-daemon`: Persistent Local Server & File Watcher (M3)

> **Goal:** `flint-daemon` runs quietly in the background, watches bound FL Studio project folders, and exposes a clean localhost REST + WebSocket API for the VST3 plugin.
>
> **Done when:** the whole create → push → pull → comment flow can be driven with `curl` + a WebSocket client, with no CLI subprocesses.

### 3.0 — API Contract First
- [ ] Define request/response/event types as `serde` structs in `flint-core` (reuse `DaemonEvent`), so the daemon and any future clients share one definition
- [ ] Add an `api_version` field to `GET /health`; the plugin refuses to talk to an incompatible daemon and shows "Please update FLint"
- [ ] Write the contract into `ARCHITECTURE.md` §5.4 (the source of truth for the C++ side)

### 3.1 — Localhost HTTP + WebSocket Server (`axum` + `tokio`)
- [ ] Run one long-lived `SyncNode` inside the daemon (the CLI keeps its start-act-stop behaviour for scripting)
- [ ] Bind to `127.0.0.1:47821` (with fallback ephemeral port) and write `{ port, token, pid, api_version }` to `%LOCALAPPDATA%\FLint\daemon.json`
- [ ] Require `X-FLint-Token` header on all requests/WS upgrades, and **reject any request with a browser `Origin` header** (stops web pages from talking to the daemon)
- [ ] Implement REST endpoints:
  - `GET /health`
  - `GET /profile`, `PUT /profile`
  - `POST /sessions` and `POST /sessions/join`
  - `GET /sessions/:id/state` (peers, timeline, compatibility report, comments, crate)
  - `POST /sessions/:id/push` and `POST /sessions/:id/pull/:snapshot_id`
  - `POST /sessions/:id/sync`, `POST /sessions/:id/prune`, `POST /sessions/:id/rotate`, `POST /sessions/:id/share`
  - `POST /sessions/:id/comments`
  - `POST /sessions/:id/crate`
  - `GET /blobs/:hash/path` (materializes blob to local cache dir for playback / drag-out)
- [ ] Long operations (push, pull, sync) return a job ID immediately and report progress over the WebSocket
- [ ] Implement `WS /events` broadcasting `peer_presence`, `snapshot_received`, `sync_progress`, `comment_added`, `crate_item_added`, and `flp_modified_on_disk`

### 3.2 — Project Watcher & Local Cache
- [ ] Watch bound project directories using the Rust `notify` crate (debounced ~500ms so mid-save writes aren't read half-written)
- [ ] On `.flp` save, automatically re-parse and compute a live "unpushed diff preview" against the folder's base version + emit `flp_modified_on_disk`
- [ ] Persist local bindings in SQLite (`%LOCALAPPDATA%\FLint\flint.db`): `binding_id → session_id → project folder` (see 4.1 for why the plugin stores a binding ID, not the session ID)
- [ ] Auto-prefetch the newest snapshot's files when a collaborator pushes (so pulls are instant and survive the author going offline)
- [ ] Run `prune` automatically with a configurable keep count (default: keep 20 versions' files)

### 3.3 — Daemon Lifecycle
- [ ] Single-instance lock (a second daemon exits and points at the first), no DB lock errors
- [ ] Auto-start on login **without admin** (per-user `HKCU\…\Run` entry or Startup-folder shortcut); opt-out in settings
- [ ] Structured logs to `%LOCALAPPDATA%\FLint\logs\` with rotation (`tracing-appender`); `flint-daemon logs` prints the path
- [ ] Clean shutdown (`POST /shutdown` + Ctrl+C) that flushes docs and blob stores

### 3.4 — Live Presence *(moved from 2.2)*
- [ ] Turn `NeighborUp/Down` events into per-session presence state, mapping endpoint IDs to display names via `peers/*/env`
- [ ] Emit `peer_presence` events; include "syncing" while a download from that peer is active
- [ ] Heartbeat timeout so a peer that crashed shows offline within ~30 s

### 3.5 — Tests
- [ ] axum integration tests for every endpoint (spin up the daemon on a random port with `NetworkMode::LocalOnly`)
- [ ] WebSocket test: two in-process daemons, a push on one produces `snapshot_received` on the other

---

## Phase 4 — VST3 Foundation: Plugin Connected to Daemon (M4)

> **Goal:** `FLint.vst3` loads inside FL Studio, remembers its session, connects non-blockingly to `flint-daemon`, and renders a live timeline, diffs, peer presence, and missing-plugin warnings.
>
> **Done when:** two people complete a push → pull → edit → push loop entirely inside FL Studio, and reopening the project reconnects automatically.

### 4.0 — Build & Install Without Admin
- [ ] Install the built plugin to the **per-user** VST3 folder (`%LOCALAPPDATA%\Programs\Common\VST3`) or a custom folder added in FL's *Options → Manage plugins*, never `Program Files`
- [ ] Confirm FL Studio finds and loads it after a plugin scan

### 4.1 — Processor State & Transport Tracking (`PluginProcessor`)
- [ ] Ensure `processBlock()` is a transparent, allocation-free stereo passthrough
- [ ] Track live `AudioPlayHead::PositionInfo` (BPM, time signature, current bar & beat, playing state) into lock-free atomics for the UI and recorder
- [ ] Persist a random **binding ID** (not the session ID) and plugin role (`Master` vs `Track`) via `getStateInformation()` / `setStateInformation()`. The session ID is a read key (ARCHITECTURE §7), so it must not travel inside `.flp` files people share. The daemon maps binding ID → session locally.
- [ ] Handle a project opened on a machine with no binding: show "This project was linked to a FLint session on another computer. Join it?"

### 4.2 — Async Daemon Client (`DaemonClient`)
- [ ] Read `%LOCALAPPDATA%\FLint\daemon.json` and auto-launch `flint-daemon` if not already running
- [ ] Check `api_version` on connect; show a clear message on mismatch
- [ ] Run HTTP requests and WebSocket event loop on a dedicated background thread (never on the JUCE Message Thread or Audio Thread)
- [ ] Marshal incoming WebSocket state updates to the UI safely via `juce::MessageManager::callAsync`
- [ ] Reconnect with backoff if the daemon restarts; never crash or block FL Studio when the daemon is missing
- [ ] Multiple FLint instances (several inserts / projects) share one daemon connection model without duplicate work

### 4.3 — Core Plugin UI (`PluginEditor`)
- [ ] Build a clean, resizable tabbed panel (`~480×600`) with dark DAW-native styling:
  - **Header bar:** Session name, Copy Invite Ticket button, live Peer Presence pills (`● Alex — Online`, `○ Sam — Offline`), Daemon connection indicator
  - **Timeline tab:** Unpushed changes banner (live auto-diff from last `Ctrl+S`), Push bar (optional note + Push button), version list showing timestamp, author, note, diff tags, missing-sample warnings, and Pull button
  - **Compatibility tab / banner:** Missing-plugin & FL version mismatch alerts with exact Channel / Mixer Insert callouts
  - **Session setup view:** "Create Session" (pick project `.flp`/folder) or "Join Session" (paste invite ticket + pick local folder)
  - **Settings:** display name, prune keep-count, "Share read-only ticket" (for seeders), "Rotate session…"
- [ ] After Pull: "Open in FL Studio" button (opens the pulled `.flp`) + "Show in Explorer"

### 4.4 — Branches & Divergence
- [ ] Draw the timeline as a graph using `parent_ids`, so parallel branches are visible
- [ ] When a push is `diverged_from_latest`, explain it: "Sam pushed v7 while you were working on v5. Your version is a parallel branch."
- [ ] "Continue from this version" action: pull it and make it the folder's base (FL projects can't be auto-merged, so the user picks which branch continues)
- [ ] Support recording a manual merge: push with two parents (`--parent a --parent b`) after combining ideas by hand

### 4.5 — Verify
- [ ] Load plugin, create/join session, save `.flp`, close & reopen FL Studio; session reconnects with zero UI stutters
- [ ] Kill the daemon while FL Studio is open: plugin shows "reconnecting", FL keeps playing

---

## Phase 5 — Bar-Synced Audio Capture & Version Previews (M5)

> **Goal:** Every pushed version can include a playable audio preview captured directly inside FL Studio, so collaborators can A/B audition versions without reloading projects.
>
> **Done when:** a preview recorded on A plays on B inside FLint, and recording causes no audio dropouts.

### 5.1 — Lock-Free Audio Capture Engine
- [ ] Add a `juce::AbstractFifo` lock-free ring buffer in `PluginProcessor`
- [ ] Support **Bar-Synced Capture**: when armed, wait for DAW playback / next bar boundary (`AudioPlayHead`), record cleanly for N bars (or until Stop), and write a compressed file + downsampled waveform peaks on a background thread
- [ ] Choose the preview format: FLAC for quality, or Opus/MP3 for size. A 2-minute stereo WAV is about 20 MB and would be re-synced to every collaborator.
- [ ] Add quick "Record Preview" toggle right next to the Push button in the Timeline tab

### 5.2 — In-Plugin Waveform Audition Player
- [ ] Render clickable waveform previews (`preview_peaks`) on each version card in the Timeline
- [ ] Stream audition audio through `PluginProcessor` (or standalone audio output when the DAW transport is stopped) so collaborators can click-to-play v2 vs v3 directly inside FLint
- [ ] Loudness-match A/B playback so the louder version doesn't win by default

---

## Phase 6 — Bar/Beat-Locked Comments & Missing-Plugin Stem Fallback (M6, V1.1)

> **Goal:** Collaborators leave mix notes pinned to exact bars/beats, and can solve missing-VST conflicts in one click.

### 6.1 — Bar/Beat-Locked Comments
- [ ] Show current playhead position (`Bar 33 • Beat 1`) in the comment input box; **"Pin Current Bar"** attaches the comment to that bar/beat
- [ ] Render comment markers visually along the version's waveform preview
- [ ] Support marking comments as **Resolved** and syncing updates live over `iroh-docs`
- [ ] Carry unresolved comments forward to child versions ("3 open notes from v4")

### 6.2 — Missing-Plugin → Stem Fallback Loop
- [ ] Next to every missing-VST warning (e.g. *"Alex is missing RC-20 on Mixer Insert 5"*), show a **"Capture Stem for Collaborator"** action
- [ ] Capture the bar-synced audio straight off that mixer insert (or guide the user to capture the soloed track) and post it to the Shared Crate tagged with BPM, bar offset, and source mixer track

---

## Phase 7 — Shared Stem / MIDI / Preset Crate (M7, V1.1)

> **Goal:** Collaborators exchange stems, vocal takes, MIDI clips, and `.fst` presets and drag them straight out of FLint onto FL Studio's Playlist or Channel Rack.

### 7.1 — Crate UI & Drag-In / Drag-Out
- [ ] Build **Crate tab** listing shared stems, MIDI files, and presets with BPM, bar length, author, and mini-waveform audition
- [ ] Support **drag-and-drop INTO** FLint (`juce::FileDragAndDropTarget`) to share any WAV, MIDI, or `.fst`/`.fxp` preset via `iroh-blobs`
- [ ] Support **drag-and-drop OUT OF** FLint (`juce::DragAndDropContainer::performExternalDragDropOfFiles`) onto FL Studio's Playlist, Channel Rack, or Mixer
- [ ] Ensure `GET /blobs/:hash/path` materializes dragged files with descriptive filenames (e.g. `Ch5_RC20_Guitar_132bpm_8bars.wav`) so FL Studio names the clip cleanly on drop
- [ ] Crate items keep their files through `prune` (already true in v0.2.1); add "remove from crate" for cleanup

---

## Phase 8 — Real-World Integration Testing (M8, part of V1.0)

- [ ] Test full loop on two separate machines across different networks (and via a seeder while one machine is offline)
- [ ] Test with real multi-hundred-MB FL Studio projects (push #2 after a small MIDI edit takes < 2 seconds and only transfers changed files)
- [ ] Verify missing-VST detection catches third-party plugins without false-flagging stock FL plugins
- [ ] Verify audio capture produces zero dropouts, zero clicks, and zero allocations on the audio thread
- [ ] Verify pulled projects open in FL Studio with no missing-sample dialog (sample-path rewrite)
- [ ] Verify pulling a version never overwrites local edits (new folder every pull; already true in v0.2.1)
- [ ] Verify clean shutdown on FL Studio close (no crashes, daemon stays healthy for other open sessions)
- [ ] **Beta test** with 2–3 real producer pairs for two weeks; log every confusion and crash as an issue

---

## Phase 9 — Packaging, CI & Release Engineering (M9, part of V1.0)

> **Done when:** a collaborator who has never seen the repo installs FLint from a single download and joins a session in under 5 minutes, without admin rights.

### 9.1 — Continuous Integration (GitHub Actions)
- [ ] `windows-latest` (MSVC) job: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test --workspace`
- [ ] Plugin job: CMake build of `FLint.vst3` (JUCE via `FetchContent`)
- [ ] Nightly job running the `--ignored` internet test
- [ ] Release job: build signed artifacts when a `v*` tag is pushed

### 9.2 — Installer & Updates
- [ ] **Per-user installer** (no admin): `flint-daemon.exe` → `%LOCALAPPDATA%\Programs\FLint\`, `FLint.vst3` → `%LOCALAPPDATA%\Programs\Common\VST3\`, auto-start entry, uninstaller
- [ ] Code-sign the executables (avoid SmartScreen "unknown publisher" warnings)
- [ ] Update check: daemon checks GitHub Releases weekly and tells the plugin "Update available"
- [ ] Data/format versioning: add `schema_version` to snapshot records; newer daemons must read older records (already done for v0.2.1 fields via `serde(default)`); document the policy

### 9.3 — Licensing & Docs
- [ ] **Decide the JUCE licence before distributing the plugin.** JUCE is dual-licensed (AGPLv3 or a commercial/free-tier licence), and the AGPL option would require the plugin to be AGPL, not the MIT declared in `Cargo.toml`. Check the current JUCE terms.
- [ ] `README.md`: install steps, collaborator quickstart, seeder setup, troubleshooting (firewall, "files unavailable" = author offline)
- [ ] Privacy note for users: what is stored where, what seeders can see, how to remove a collaborator

---

## Phase 10 — Stretch Roadmap (Post-V1.1)

- [ ] **Client-side encryption / blind seeders** (design in 2.4)
- [ ] **macOS support:** daemon (already cross-platform), AU + VST3 plugin, notarization, Apple Silicon builds, FL Studio Mac paths
- [ ] **System Tray / Companion App:** tray icon to manage the daemon, view the sync queue, and pull/open versions before launching FL Studio
- [ ] **Notifications:** Windows toast when a collaborator pushes or comments
- [ ] **Live P2P Audio Streaming:** low-latency Opus stream over `iroh` QUIC datagrams from the Master bus to a collaborator's FLint plugin (open-source Audiomovers alternative)
- [ ] **Cross-DAW Expansion:** Shared Crate + Audio Preview + Bar Comments for Ableton Live, Reaper, and Logic (plus an Ableton `.als` gzipped-XML parser alongside `flint-flp`)
- [ ] **Open-Source `flint-flp` Crate:** publish `flint-flp` on crates.io as a reusable Rust `.flp` parser (after the fixture corpus and fuzzing in 1.4)

---

## Change Log

| Version | Date | Highlights |
|---|---|---|
| 0.2.1 | 2026-11-25 | 16 review fixes: correct parents via `.flint` records, on-demand downloads, zip folder structure, missing-sample reports, sample-path rewrite, streaming ingest, GC + `prune`, read-only seeders, `rotate`, `--hide-ip`, arrangement + plugin-setting diffs, smarter channel/pattern matching, embedded-sample detection, FL plugin database scan, saved profile, pinned Iroh versions, offline tests |
| 0.2.0 | 2026-11 | Direction C: Rust workspace, `.flp` parser + musical diff, VST scanner, Iroh sync engine, seeder, JUCE CMake migration |
| 0.1 | 2026-11 | Node.js + Hypercore sidecar prototype (`sidecar/`, to be archived) |
