# FLint — Development Checklist
### Async DAW Collaboration Plugin for FL Studio

---

## How to Use This Checklist
- Work through milestones in order — each builds on the last
- Check off tasks as you complete them
- The **Sidecar** (Node.js) and **Plugin** (C++) tracks can be developed in parallel once the CLI contract is agreed upon (M1 boundary)
- When stuck, the architecture diagram and CLI interface spec in the original V1 doc are your source of truth

---

## Phase 0 — Environment Setup

### 0.1 — Node.js Sidecar Environment
- [ ] Install Node.js 20+ from nodejs.org
- [ ] Create the `flint/sidecar/` folder
- [ ] Run `npm init -y` inside it to create `package.json`
- [ ] Install dependencies:
  ```bash
  npm install hypercore hyperswarm corestore b4a minimist
  ```
- [ ] Verify install: `node -e "require('hypercore'); console.log('ok')"` — should print `ok`
- [ ] Create a scratch file `test.js` and confirm you can run `node test.js` without errors

### 0.2 — JUCE / C++ Plugin Environment
- [ ] Download JUCE 7 from juce.com — extract to a stable location (e.g. `C:/JUCE`)
- [ ] Download the VST3 SDK — place it somewhere accessible
- [ ] Install CMake 3.22+ from cmake.org
- [ ] Install Visual Studio 2022 (Community is fine) with the **Desktop development with C++** workload
- [ ] Open the Projucer (ships with JUCE), create a new **Audio Plugin** project named `FLint`
- [ ] In Projucer settings, set Plugin Formats to **VST3 only**
- [ ] Set the plugin type to **Effect** (not Instrument) — FLint doesn't process audio
- [ ] Export the Projucer project and confirm it builds with zero errors in Visual Studio

### 0.3 — Repo Structure
- [ ] Create the top-level folder `flint/`
- [ ] Inside it, create:
  ```
  flint/
  ├── plugin/       ← JUCE project goes here
  ├── sidecar/      ← Node.js CLI goes here
  └── README.md
  ```
- [ ] Initialise a git repo: `git init` — commit this empty structure as your first commit

---

## Phase 1 — Sidecar: Core Feed Logic (M1)

> Goal: `init`, `push`, and `pull` commands work between two folders on the same machine.

### 1.1 — Entry Point
- [x] Create `sidecar/index.js`
- [x] Use `minimist` to parse `process.argv.slice(2)` — store the result as `args`
- [x] Read `args._[0]` as the command name (e.g. `"init"`, `"push"`, `"pull"`)
- [x] Add a `switch` statement routing to stub functions for each command
- [x] All output must be `console.log(JSON.stringify({...}))` — the plugin will parse stdout as JSON

### 1.2 — Init Command
- [x] Create `sidecar/feed.js`
- [x] In `feed.js`, write an async `initFeed(storageDir)` function that:
  - [x] Creates a `Corestore` at `storageDir`
  - [x] Creates a new `Hypercore` (writable) from that store
  - [x] Awaits `core.ready()`
  - [x] Returns the feed's `key` as a hex string: `core.key.toString('hex')`
- [x] Back in `index.js`, wire the `init` command to call `initFeed` and print:
  ```json
  { "session_id": "<first 6 chars of key>", "feed_key": "<full hex key>" }
  ```
- [x] Test: `node index.js init` — should print a JSON object with no errors

### 1.3 — Push Command
- [x] Add `pushVersion(storageDir, filePath, note)` to `feed.js`
- [x] This function should:
  - [x] Open the existing Corestore + Hypercore (by loading from same `storageDir`)
  - [x] Read the zip file as a Buffer: `fs.readFileSync(filePath)`
  - [x] Append an entry to the feed: `core.append(Buffer.from(JSON.stringify({ note, file: fileBuffer.toString('base64') })))`
  - [x] Return `{ success: true, version: core.length }`
- [x] Wire the `push` command in `index.js`: accepts `--session`, `--file`, `--note` flags
- [x] Test: create a dummy `test.zip` (can be any file renamed), then:
  ```bash
  node index.js push --session abc123 --file ./test.zip --note "first push"
  ```
  Should print `{ "success": true, "version": 1 }`

### 1.4 — Pull Command
- [x] Add `pullLatest(storageDir, outDir)` to `feed.js`
- [x] This function should:
  - [x] Open the Hypercore from `storageDir`
  - [x] Get the last entry: `core.get(core.length - 1)`
  - [x] Parse the JSON, decode the base64 file buffer
  - [x] Write the buffer to `outDir/project.zip`
  - [x] Return `{ success: true, file: outDir + '/project.zip', note, version }`
- [x] Wire the `pull` command in `index.js`: accepts `--session`, `--out` flags
- [x] Test end-to-end locally:
  ```bash
  node index.js init
  node index.js push --session abc123 --file ./test.zip --note "test"
  node index.js pull --session abc123 --out ./incoming
  ```
  The file should appear in `./incoming/project.zip`

### 1.5 — Log Command
- [x] Add `getLog(storageDir)` to `feed.js` — reads all entries and returns array of `{ version, timestamp, note }`
- [x] Wire the `log` command in `index.js`
- [x] Test: `node index.js log --session abc123` — should print a JSON array of versions

### 1.6 — Status Command (stub)
- [x] Add a `status` command stub that returns `{ peer_online: false, peer_version: 0, local_version: N }`
- [x] This will be fully implemented in Phase 2 once Hyperswarm is added

---

## Phase 2 — Sidecar: P2P Replication over the Internet (M2)

> Goal: Two machines on different networks can sync feeds without a server.

### 2.1 — Understand Hyperswarm
- [x] Read the Hyperswarm README on GitHub before writing any code
- [x] Key concept: you join a swarm using a **topic** (a 32-byte Buffer). Both peers use the same topic to find each other via DHT
- [x] For FLint, the topic will be derived from the session's feed key

### 2.2 — Create swarm.js
- [x] Create `sidecar/swarm.js`
- [x] Write an async `joinSwarm(core)` function that:
  - [x] Creates a new `Hyperswarm` instance
  - [x] Calls `swarm.join(core.discoveryKey)` — Hypercore exposes `discoveryKey` as the swarm topic
  - [x] Listens for `swarm.on('connection', (conn) => { core.replicate(conn) })` — this wires replication automatically
  - [x] Returns the swarm instance (so the caller can destroy it later)
- [x] Import and call `joinSwarm` inside `pushVersion` after appending — the feed will replicate to connected peers
- [x] Import and call `joinSwarm` inside `pullLatest` before reading — wait briefly for peers to connect (use `setTimeout` + `Promise` for ~3 seconds)

### 2.3 — Test Over the Internet
- [ ] Set up two machines (or two terminal sessions, one behind a VPN to simulate different networks)
- [ ] On Machine A: `node index.js init` — note the `feed_key`
- [ ] On Machine B: hardcode the feed_key, open the same Corestore, replicate
- [ ] On Machine A: push a version
- [ ] On Machine B: pull — verify the zip arrives

### 2.4 — Status Command (real implementation)
- [ ] Update the `status` command to join the swarm, wait for connection events, and report `peer_online: true/false`
- [ ] Report `peer_version` by reading the peer's replicated core length
- [ ] Test: run `status` on both machines simultaneously

---

## Phase 3 — JUCE Plugin: Basic Panel (M3)

> Goal: A panel with FLint's UI renders inside FL Studio as a VST3 plugin.

### 3.1 — Understand the JUCE files
- [ ] Open `plugin/Source/PluginProcessor.cpp` — this is where audio processing logic goes (you won't use it much, but don't delete it)
- [ ] Open `plugin/Source/PluginEditor.cpp` — this is where your UI lives
- [ ] The `PluginEditor` constructor is called when FL Studio opens the plugin window

### 3.2 — Design the Layout
- [ ] In `PluginEditor.h`, declare the following member components:
  ```cpp
  juce::Label sessionLabel;
  juce::TextButton copyButton { "Copy" };
  juce::Label peerStatusLabel;
  juce::TextButton pushButton { "Push" };
  juce::TextButton pullButton { "Pull" };
  juce::ListBox versionList;
  ```
- [ ] In `PluginEditor.cpp` constructor, add each component with `addAndMakeVisible()`

### 3.3 — Implement paint() and resized()
- [ ] In `paint()`, fill the background: `g.fillAll(getLookAndFeel().findColour(juce::ResizableWindow::backgroundColourId))`
- [ ] In `resized()`, use `juce::Rectangle<int>` to manually position each component — lay them out top to bottom matching the wireframe in the spec
- [ ] Set a reasonable default plugin size in the constructor: `setSize(300, 400)`

### 3.4 — Test in FL Studio
- [ ] Build the VST3 in Release mode (Visual Studio → Build → Build Solution)
- [ ] Find the `.vst3` file (CMake output, usually in `build/` folder)
- [ ] Copy it to `C:/Program Files/Common Files/VST3/`
- [ ] Open FL Studio → Mixer → click an insert → add effect → scan for VST3s → load FLint
- [ ] Confirm the panel appears with all buttons and labels visible — no crashes

---

## Phase 4 — Plugin: File Watcher (M4)

> Goal: Plugin detects when the project zip is saved.

### 4.1 — Set Up the Watched Folder
- [ ] In `PluginProcessor.h`, add a member: `juce::File watchedFolder;`
- [ ] In the constructor, set it to a fixed path for now: `juce::File::getSpecialLocation(juce::File::userDocumentsDirectory).getChildFile("FLint")`
- [ ] Create this folder on disk if it doesn't exist: `watchedFolder.createDirectory()`

### 4.2 — Implement FileSystemWatcher
- [ ] In `PluginProcessor.h`, inherit from `juce::FileSystemWatcher::Listener` (or use a lambda callback if your JUCE version supports it)
- [ ] Declare a `juce::FileSystemWatcher watcher` member
- [ ] In the constructor, call `watcher.addFolder(watchedFolder)` and `watcher.addListener(this)`
- [ ] Implement the callback `fileSystemChanged()` — for now just log to the JUCE debugger: `DBG("Zip changed: " + file.getFullPathName())`
- [ ] Filter for only `.zip` files inside the callback

### 4.3 — Test
- [ ] Build and load in FL Studio
- [ ] Manually copy/modify a `.zip` file in the watched folder
- [ ] Confirm the debug log fires (use the JUCE debugger or a debug build in Visual Studio with breakpoints)

---

## Phase 5 — Plugin: Subprocess Calls to Sidecar (M5)

> Goal: Plugin calls the sidecar CLI and reads its JSON output.

### 5.1 — Bundle the Sidecar
- [ ] Install Node.js's `pkg` tool: `npm install -g pkg`
- [ ] In `sidecar/package.json`, add: `"bin": { "flint-sidecar": "./index.js" }`
- [ ] Run: `pkg . --target node20-win-x64 --output flint-sidecar.exe`
- [ ] Place `flint-sidecar.exe` next to the plugin's `.vst3` file (or in a known relative path)

### 5.2 — Invoke from Plugin
- [ ] In `PluginProcessor.cpp`, write a helper function:
  ```cpp
  juce::String callSidecar(juce::StringArray args) {
      juce::ChildProcess process;
      juce::String exe = /* path to flint-sidecar.exe */;
      process.start(exe + " " + args.joinIntoString(" "));
      process.waitForProcessToFinish(10000); // 10s timeout
      return process.readAllProcessOutput();
  }
  ```
- [ ] Test by calling `callSidecar({"log", "--session", "abc123"})` and logging the output

### 5.3 — Parse JSON Output
- [ ] Use JUCE's built-in JSON parser: `juce::JSON::parse(outputString)`
- [ ] Write a helper that calls sidecar and returns a `juce::var` (JUCE's dynamic type):
  ```cpp
  juce::var callSidecarJSON(juce::StringArray args) {
      auto output = callSidecar(args);
      return juce::JSON::parse(output);
  }
  ```
- [ ] Test: parse the `log` output and extract the `version` field from the first entry

---

## Phase 6 — Full Push Flow End to End (M6)

> Goal: Producer saves zip in FL Studio → clicks Push → peer receives it.

### 6.1 — Wire Push Button
- [ ] In `PluginEditor.cpp`, add a click handler to the Push button:
  ```cpp
  pushButton.onClick = [this]() { processor.doPush("optional note here"); };
  ```
- [ ] In `PluginProcessor.cpp`, implement `doPush(note)`:
  - [ ] Find the latest `.zip` in the watched folder
  - [ ] Call `callSidecarJSON({"push", "--session", sessionId, "--file", zipPath, "--note", note})`
  - [ ] Parse the response, log the new version number

### 6.2 — Show Feedback
- [ ] After a successful push, update the version list in the UI
- [ ] Show a brief status message: "Pushed version 3" in the `peerStatusLabel`

### 6.3 — End-to-End Test
- [ ] Machine A: Create a zip file in the FLint watched folder
- [ ] Machine A: Click Push in FL Studio
- [ ] Machine B: Wait for replication (~5–30 seconds depending on NAT)
- [ ] Verify the sidecar on Machine B has received the new version (check via `log` command)

---

## Phase 7 — Full Pull Flow End to End (M7)

> Goal: Collaborator clicks Pull → zip lands in incoming folder → they open it in FL Studio.

### 7.1 — Wire Pull Button
- [ ] In `PluginEditor.cpp`, add a click handler to the Pull button:
  ```cpp
  pullButton.onClick = [this]() { processor.doPull(); };
  ```
- [ ] In `PluginProcessor.cpp`, implement `doPull()`:
  - [ ] Call `callSidecarJSON({"pull", "--session", sessionId, "--out", incomingFolderPath})`
  - [ ] Parse the response
  - [ ] Show a dialog or label: `"Ready — open FLint/incoming/project.zip in FL Studio"`

### 7.2 — Incoming Folder
- [ ] Create an `incoming/` subfolder inside the FLint watched folder automatically on startup
- [ ] After pull, open the folder in Windows Explorer for convenience: `juce::File(incomingPath).revealToUser()`

### 7.3 — End-to-End Test
- [ ] Machine B: Click Pull in FL Studio
- [ ] Verify `FLint/incoming/project.zip` appears on Machine B's disk
- [ ] Open the zip directly in FL Studio — verify all samples load (no missing sample warnings)

---

## Phase 8 — Peer Online/Offline Status (M8)

> Goal: The "Peer: ● Online" indicator in the plugin reflects real connectivity.

### 8.1 — Background Status Polling
- [ ] In `PluginProcessor.cpp`, add a `juce::Timer` that fires every 10 seconds
- [ ] In `timerCallback()`, call `callSidecarJSON({"status", "--session", sessionId})`
- [ ] Parse `peer_online` boolean from the result
- [ ] Store it as a member variable: `bool peerOnline`

### 8.2 — Update the UI
- [ ] After each poll, call `juce::MessageManager::callAsync([this]() { editor->updatePeerStatus(peerOnline); })`
- [ ] In `PluginEditor`, implement `updatePeerStatus(bool online)`:
  - [ ] Set `peerStatusLabel.setText(online ? "● Online" : "○ Offline", ...)`
  - [ ] Set label colour: green for online, grey for offline

### 8.3 — Test
- [ ] With both machines running, verify the Online indicator lights up within 10 seconds
- [ ] Close the sidecar on one machine, verify the other shows Offline within 10–20 seconds

---

## Phase 9 — Version History List (M9)

> Goal: The version list in the plugin populates from sidecar `log` output.

### 9.1 — Fetch Version Log
- [ ] On plugin startup, call `callSidecarJSON({"log", "--session", sessionId})` and store the result
- [ ] Also call this after every successful push or pull to refresh the list

### 9.2 — Display in ListBox
- [ ] Implement `juce::ListBoxModel` — override `getNumRows()` and `paintListBoxItem()`
- [ ] In `paintListBoxItem()`, draw:
  - Line 1: formatted timestamp (e.g. "Today 2:57am")
  - Line 2: the note string (e.g. "updated the drop")
- [ ] Wire the model to the `versionList` ListBox

### 9.3 — Session ID Display + Copy Button
- [ ] Show the session ID in `sessionLabel`
- [ ] Wire `copyButton.onClick` to: `juce::SystemClipboard::copyTextToClipboard(sessionId)`

---

## Phase 10 — Real FL Studio Integration Test (M10)

> Goal: Full workflow tested with an actual FL Studio session between two machines.

### 10.1 — Create a Real FL Studio Project
- [ ] In FL Studio, create a short project (a few patterns, some samples loaded)
- [ ] File → Export → Zipped Loop → save to `FLint/` watched folder
- [ ] Open the zip directly in FL Studio (File → Open) — confirm it works

### 10.2 — Test the Full Workflow
- [ ] Machine A: Push the initial zip via the FLint plugin panel
- [ ] Machine B: Pull via FLint panel, open `incoming/project.zip` in FL Studio
- [ ] Machine B: Make a change (add a pattern), Ctrl+S to save back to the zip
- [ ] Machine B: Push via FLint panel
- [ ] Machine A: Wait for Online status, click Pull, open the zip

### 10.3 — Edge Cases to Verify
- [ ] What happens if you push while the other peer is offline? (Should queue and replicate when they come online)
- [ ] What happens if the zip is corrupt or incomplete? (Add error handling in the sidecar)
- [ ] What happens if two people push at the same time? (Each peer's feed is independent — both versions are preserved)

### 10.4 — Fix Any Issues Found
- [ ] Log all bugs found during M10 testing, fix them before declaring V1 complete

---

## Final Checklist Before Shipping V1

- [ ] Both machines tested on real FL Studio projects (not dummy zips)
- [ ] Plugin loads in FL Studio without any error/crash on startup
- [ ] Push flow works end-to-end over the internet
- [ ] Pull flow works end-to-end over the internet
- [ ] Peer status indicator is accurate within ~15 seconds
- [ ] Version list shows all historical versions
- [ ] Session ID is copyable via the Copy button
- [ ] Plugin does not crash on FL Studio close
- [ ] Sidecar process is cleaned up when plugin is unloaded (`ChildProcess` destructor or explicit kill)
- [ ] README.md explains setup steps for a new collaborator

---

## Quick Reference: Sidecar CLI Contract

> This is the agreed interface between the C++ plugin and the Node.js sidecar. Both sides must match exactly.

```bash
node index.js init
# → { "session_id": "abc123", "feed_key": "a1b2c3..." }

node index.js push --session abc123 --file ./project.zip --note "updated drop"
# → { "success": true, "version": 3 }

node index.js pull --session abc123 --out ./incoming/
# → { "success": true, "file": "./incoming/project.zip", "note": "added vocals", "version": 2 }

node index.js status --session abc123
# → { "peer_online": true, "peer_version": 2, "local_version": 3 }

node index.js log --session abc123
# → [{ "version": 1, "timestamp": "...", "note": "first version" }, ...]
```

All commands output **only** JSON to stdout. Errors go to stderr and also follow JSON format:
```json
{ "success": false, "error": "description of what went wrong" }
```
