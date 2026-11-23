# FLint — Development Checklist (v2 / Direction C)
### DAW-Aware, Pure-P2P Collaboration for FL Studio (Rust + Iroh + JUCE VST3)

> See [ARCHITECTURE.md](./ARCHITECTURE.md) for the full system design, data model, and IPC contract.

---

## How to Use This Checklist
- Build in **vertical slices** — each phase produces something real and testable on its own.
- **Phases 1–3** build the Rust daemon (`flint-flp`, `flint-sync`, `flint-daemon`). You can test the entire `.flp` diffing, missing-plugin checking, and P2P sync workflow via CLI before touching C++.
- **Phases 4–7** build the JUCE VST3 plugin (CMake) connected to the daemon over localhost HTTP/WebSocket, adding the DAW-native superpowers (audio previews, bar-locked comments, drag-and-drop stem crate).
- **Phase 8** validates the complete workflow on real FL Studio projects across two machines.

---

## Phase 0 — Environment & Workspace Setup

### 0.1 — Rust Workspace (`daemon/`)
- [x] Install Rust toolchain (`rustup`, stable `cargo`) and verify `cargo --version`
- [x] Create Cargo workspace at `daemon/Cargo.toml` with member crates:
  - `crates/flint-core` (shared types: `ProjectManifest`, `MusicalDiff`, `Snapshot`, `Comment`, `CrateItem`)
  - `crates/flint-flp` (`.flp` binary parser & diff engine)
  - `crates/flint-vst-scan` (installed plugin scanner)
  - `crates/flint-sync` (`iroh` P2P layer)
  - `crates/flint-daemon` (HTTP/WS daemon + CLI + `--seeder` mode)
- [x] Verify `cargo check --workspace` succeeds cleanly

### 0.2 — Migrate JUCE Plugin to CMake (`plugin/`)
- [x] Verify CMake 3.22+, Visual Studio C++ workload, and JUCE path (or fetch JUCE via `FetchContent` for reproducible builds)
- [x] Add `plugin/CMakeLists.txt` targeting `VST3` + `Standalone` (for rapid UI testing outside FL Studio)
- [x] Confirm `FLint.vst3` builds cleanly via CMake in both Debug and Release
- [x] Add `daemon/target/` and CMake `build/` directories to `.gitignore`
- [ ] Move/archive legacy `sidecar/` Node prototype once Phase 2 passes

---

## Phase 1 — `flint-flp`: `.flp` Parser, Musical Diffs & Plugin Scanner (M1)

> **Goal:** Run a CLI command against two real `.flp` files and get an accurate musical diff + missing-plugin report. *(Proves the killer differentiator immediately.)*

### 1.1 — `.flp` Binary Parser (`flint-flp`)
- [x] Parse `FLhd` header (`format`, `num_channels`, `ppq`) and validate magic bytes
- [x] Parse `FLdt` chunk and implement the 4-band TLV event reader (`0..=63` byte, `64..=127` word, `128..=191` dword, `192..=255` varint length-prefixed data)
- [x] Extract core project metadata: FL Studio version string, BPM (including fine tempo), time signature, title
- [x] Extract **Channels**: index, name, generator/plugin name, sample file path (resolving `%FLStudioUserData%` / relative paths where applicable)
- [x] Extract **Mixer Inserts**: index, name, FX slot plugin names (handling Fruity Wrapper VST2/VST3 names)
- [x] Extract **Patterns**: index, name, note count + deterministic content hash of pattern notes
- [x] Support reading `.flp` directly *or* from inside a Zipped Loop `.zip` without manual extraction
- [x] Write unit tests and verify against real `.flp` / `.zip` projects *(verified on `first melodic techno.flp` (FL 24.2.2) and `im on team cece.zip` (macOS FL 26.1.5 with Serum 2 + ShaperBox 3) + 6 automated unit tests)*

### 1.2 — Semantic Diff Engine (`flint-flp`)
- [x] Implement `diff_manifests(old, new) -> MusicalDiff`
- [x] Detect and format human-readable summaries for:
  - Tempo / FL version changes
  - Added / removed / renamed channels and plugins
  - Added / removed / reordered FX on mixer inserts
  - Added / removed / modified patterns (including note count deltas)
  - Added / removed audio sample files
- [x] CLI smoke test: `flint-daemon inspect project_v1.flp` and `flint-daemon diff project_v1.flp project_v2.flp`

### 1.3 — Installed Plugin Scanner (`flint-vst-scan`)
- [x] Scan Windows VST3 (`C:\Program Files\Common Files\VST3`), common VST2 paths, and configurable custom folders
- [x] Normalize plugin names (strip `.vst3`/`.dll`, handle architecture suffixes like `(x64)`) + maintain FL Studio stock plugin allowlist so native FL plugins never trigger false-positive warnings
- [x] Implement `check_compatibility(manifest, peer_env) -> CompatibilityReport` listing missing VSTs with their exact Channel / Mixer Insert location and FL version mismatch warnings

---

## Phase 2 — `flint-sync`: Pure-P2P Multi-Writer Engine with Iroh (M2)

> **Goal:** Two machines on different networks exchange deduplicated `.flp` snapshots, samples, comments, and crate items via pure P2P — with multi-writer support and an optional always-on seeder.

### 2.1 — Iroh Core + Blobs (`iroh` + `iroh-blobs`)
- [ ] Set up persistent `iroh::Endpoint` with stored Ed25519 secret key in `%LOCALAPPDATA%\FLint\`
- [ ] Initialize persistent `iroh-blobs` store (BLAKE3 content-addressed storage)
- [ ] Implement `ingest_project_assets(project_dir, manifest)` — hashes `.flp` and each referenced sample individually into `iroh-blobs` (zero zipping, automatic deduplication)
- [ ] Implement `materialize_snapshot(snapshot, target_dir)` — fetches only missing blobs from peers and writes files cleanly without clobbering active unsaved files

### 2.2 — Multi-Writer Session Sync (`iroh-docs` + `iroh-gossip`)
- [ ] Implement `create_session()` → creates an `iroh-docs` namespace and returns a shareable invite ticket
- [ ] Implement `join_session(ticket)` → joins namespace, subscribes to document sync and `iroh-gossip` presence topic
- [ ] Publish local environment (`peers/<node_id>/env`) on join and startup
- [ ] Implement multi-writer `push_snapshot()` writing `snapshots/<ulid>_<author>` with `parent_ids`, `manifest`, `diff_summary`, `flp_blob`, `assets`, and optional `preview_audio_blob`
- [ ] Implement `add_comment()` (`comments/<snapshot_id>/<ulid>`) and `add_crate_item()` (`crate/<ulid>`)
- [ ] Implement live peer presence tracking via `iroh-gossip` (online / offline / syncing)

### 2.3 — Optional Always-On Seeder Mode
- [ ] Add `flint-daemon --seeder --data-dir <path>` headless mode
- [ ] Add `flint-daemon seed add <ticket>` to pin a session on a VPS / Raspberry Pi / home PC for 24/7 offline async replication
- [ ] **Two-Machine Internet Test:** Push a real FL Studio project with samples from Machine A → verify only changed files transfer on push #2 → verify concurrent pushes from A and B both appear cleanly without clobbering

---

## Phase 3 — `flint-daemon`: Persistent Local Server & File Watcher (M3)

> **Goal:** `flint-daemon` runs quietly in the background, watches bound FL Studio project folders, and exposes a clean localhost REST + WebSocket API for the VST3 plugin.

### 3.1 — Localhost HTTP + WebSocket Server (`axum` + `tokio`)
- [ ] Bind to `127.0.0.1:47821` (with fallback ephemeral port) and write `{ port, token }` to `%LOCALAPPDATA%\FLint\daemon.json`
- [ ] Require `X-FLint-Token` header on all requests/WS upgrades for local security
- [ ] Implement REST endpoints:
  - `GET /health`
  - `POST /sessions` and `POST /sessions/join`
  - `GET /sessions/:id/state` (peers, timeline, compatibility report, comments, crate)
  - `POST /sessions/:id/push` and `POST /sessions/:id/pull/:snapshot_id`
  - `POST /sessions/:id/comments`
  - `POST /sessions/:id/crate`
  - `GET /blobs/:hash/path` (materializes blob to local cache dir for playback / drag-out)
- [ ] Implement `WS /events` broadcasting `peer_presence`, `snapshot_received`, `sync_progress`, `comment_added`, `crate_item_added`, and `flp_modified_on_disk`

### 3.2 — Project Watcher & Local Cache
- [ ] Watch bound project directories using the Rust `notify` crate (debounced ~500ms so mid-save writes aren't read half-written)
- [ ] On `.flp` save, automatically re-parse and compute a live "unpushed diff preview" + emit `flp_modified_on_disk` over WebSocket
- [ ] Persist local session-to-folder bindings in SQLite (`%LOCALAPPDATA%\FLint\flint.db`)
- [ ] Verify clean shutdown and single-instance lock (no duplicate daemons, no DB lock errors)

---

## Phase 4 — VST3 Foundation: Plugin Connected to Daemon (M4)

> **Goal:** `FLint.vst3` loads inside FL Studio, remembers its session inside the `.flp`, connects non-blockingly to `flint-daemon`, and renders live timeline, diffs, peer presence, and missing-plugin warnings.

### 4.1 — Processor State & Transport Tracking (`PluginProcessor`)
- [ ] Ensure `processBlock()` is a transparent, allocation-free stereo passthrough
- [ ] Track live `AudioPlayHead::PositionInfo` (BPM, time signature, current bar & beat, playing state) into lock-free atomics for the UI and recorder
- [ ] Implement `getStateInformation()` / `setStateInformation()` to persist `session_id` and plugin role (`Master` vs `Track`) inside the FL Studio project

### 4.2 — Async Daemon Client (`DaemonClient`)
- [ ] Read `%LOCALAPPDATA%\FLint\daemon.json` and auto-launch `flint-daemon` if not already running
- [ ] Run HTTP requests and WebSocket event loop on a dedicated background thread (never on the JUCE Message Thread or Audio Thread)
- [ ] Marshal incoming WebSocket state updates to the UI safely via `juce::MessageManager::callAsync`

### 4.3 — Core Plugin UI (`PluginEditor`)
- [ ] Build clean, resizable tabbed panel (`~480×600`) with dark DAW-native styling:
  - **Header bar:** Session name, Copy Invite Ticket button, live Peer Presence pills (`● Alex — Online`, `○ Sam — Offline`), Daemon connection indicator
  - **Timeline tab:** Unpushed changes banner (live auto-diff from last `Ctrl+S`!), Push bar (optional note + Push button), chronological/branching version list showing timestamp, author, note, auto-generated musical diff tags, and Pull button
  - **Compatibility tab / banner:** Missing-plugin & FL version mismatch alerts with exact Channel / Mixer Insert callouts
  - **Session setup view:** "Create Session" (pick project `.flp`/folder) or "Join Session" (paste invite ticket + pick local folder)
- [ ] Verify in FL Studio: load plugin, create/join session, save `.flp`, close & reopen FL Studio — confirm session reconnects automatically with zero UI stutters

---

## Phase 5 — VST3 Superpower #1: Bar-Synced Audio Capture & Version Previews (M5)

> **Goal:** Every pushed version can include a playable audio preview captured directly inside FL Studio, so collaborators can A/B audition versions without reloading projects.

### 5.1 — Lock-Free Audio Capture Engine
- [ ] Add a `juce::AbstractFifo` lock-free ring buffer in `PluginProcessor`
- [ ] Support **Bar-Synced Capture**: when armed, wait for DAW playback / next bar boundary (`AudioPlayHead`), record cleanly for N bars (or until Stop), and write WAV/FLAC + downsampled waveform peaks on a background thread
- [ ] Add quick "Record Preview" toggle right next to the Push button in the Timeline tab

### 5.2 — In-Plugin Waveform Audition Player
- [ ] Render clickable waveform previews (`preview_peaks`) on each version card in the Timeline
- [ ] Stream audition audio through `PluginProcessor` (or standalone audio output when playing preview while DAW transport is stopped) so collaborators can click-to-play v2 vs v3 directly inside FLint

---

## Phase 6 — VST3 Superpower #2: Bar/Beat-Locked Comments & Missing-Plugin Stem Fallback (M6)

> **Goal:** Collaborators leave mix notes pinned to exact bars/beats, and can solve missing-VST conflicts in one click.

### 6.1 — Bar/Beat-Locked Comments
- [ ] Show current playhead position (`Bar 33 • Beat 1`) in the comment input box — clicking **"Pin Current Bar"** attaches the comment to the exact DAW bar/beat
- [ ] Render comment markers visually along the version's waveform preview
- [ ] Support marking comments as **Resolved** and syncing updates live over `iroh-docs`

### 6.2 — Missing-Plugin → Stem Fallback Loop
- [ ] Next to every missing-VST warning (e.g. *"Alex is missing RC-20 on Mixer Insert 5"*), show a **"Capture Stem for Collaborator"** action
- [ ] Captures the bar-synced audio straight off that mixer insert (or guides you to capture the soloed track) and automatically posts it to the Shared Crate tagged with BPM, bar offset, and source mixer track

---

## Phase 7 — VST3 Superpower #3: Shared Stem / MIDI / Preset Crate (M7)

> **Goal:** Collaborators exchange stems, vocal takes, MIDI clips, and `.fst` presets and drag them straight out of FLint onto FL Studio's Playlist or Channel Rack.

### 7.1 — Crate UI & Drag-In / Drag-Out
- [ ] Build **Crate tab** listing shared stems, MIDI files, and presets with BPM, bar length, author, and mini-waveform audition
- [ ] Support **drag-and-drop INTO** FLint (`juce::FileDragAndDropTarget`) to share any WAV, MIDI, or `.fst`/`.fxp` preset instantly via `iroh-blobs`
- [ ] Support **drag-and-drop OUT OF** FLint (`juce::DragAndDropContainer::performExternalDragDropOfFiles`) directly onto FL Studio's Playlist, Channel Rack, or Mixer!
- [ ] Ensure `GET /blobs/:hash/path` materializes dragged files with clean, descriptive filenames (e.g. `Ch5_RC20_Guitar_132bpm_8bars.wav`) so FL Studio names the clip cleanly on drop

---

## Phase 8 — Real-World Integration Testing & V1.0 Ship Checklist (M8)

- [ ] Test full loop on two separate machines across different networks (and via `--seeder` while one machine is offline)
- [ ] Test with real multi-hundred-MB FL Studio projects (verify push #2 after a small MIDI edit takes < 2 seconds and only transfers delta bytes)
- [ ] Verify missing-VST detection accurately catches third-party plugins without false-flagging stock FL plugins
- [ ] Verify audio capture produces zero dropouts, zero clicks, and zero allocations on the audio thread
- [ ] Verify drag-and-drop of stems and MIDI from FLint directly into FL Studio Playlist / Channel Rack
- [ ] Verify pulling a version never overwrites unsaved local edits (uses versioned workspace folder + safe prompt)
- [ ] Verify clean shutdown on FL Studio close (no crashes, daemon remains healthy for other open sessions)
- [ ] Write `README.md` with install steps, collaborator quickstart, and optional self-hosted `--seeder` setup

---

## Phase 9 — V2 Stretch Roadmap (Post-V1.0)

- [ ] **Live P2P Audio Streaming:** Low-latency Opus/FLAC stream over `iroh` QUIC datagrams from Master bus to collaborator's FLint plugin (open-source Audiomovers alternative)
- [ ] **Cross-DAW Expansion:** Make the Shared Crate + Audio Preview + Bar Comments work across Ableton Live, Reaper, and Logic (and add an Ableton `.als` gzipped-XML parser alongside `flint-flp`)
- [ ] **System Tray / Companion App:** Lightweight tray icon to manage background `flint-daemon`, view sync queue, and pull/open versions even before launching FL Studio
- [ ] **Open-Source `flint-flp` Crate:** Publish `flint-flp` independently on crates.io as a reusable Rust `.flp` parser
