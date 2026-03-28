# osc-obs-bridge Implementation Plan

## Overview

A lightweight Rust binary that bridges OSC (from TouchOSC) to OBS WebSocket v5. Runs headlessly with a system tray icon for status. Replaces the bloated Electron-based OSC-for-OBS app.

## Architecture

```
TouchOSC (tablet)
    |  UDP (OSC)
    v
osc-obs-bridge (this program)
    |  WebSocket
    v
OBS Studio
```

Single async binary using tokio. Three concurrent tasks:
1. **OSC listener** -- UDP socket receiving OSC messages
2. **OBS client** -- WebSocket connection to OBS, handles commands + events
3. **Tray icon** -- System tray with status indicator + right-click menu

Communication between tasks via tokio channels.

## Dependencies (Cargo.toml)

| Crate | Purpose |
|---|---|
| `tokio` | Async runtime |
| `obws` | OBS WebSocket v5 client (typed API) |
| `rosc` | OSC message encoding/decoding |
| `tray-icon` + `muda` | System tray icon + menu |
| `serde` + `serde_json` | Config file parsing |
| `tracing` + `tracing-subscriber` | Structured logging to file + stdout |
| `image` | Tray icon image loading (required by tray-icon) |
| `dirs` | Platform-appropriate log/config directory |

## File Structure

```
osc-obs-bridge/
  Cargo.toml
  config.json              # default config (created on first run)
  src/
    main.rs                # entry point, tray icon event loop, spawns tasks
    config.rs              # config loading/defaults
    osc.rs                 # OSC UDP listener + sender
    obs.rs                 # OBS WebSocket client, command handling, event listening
    bridge.rs              # message types shared between OSC and OBS tasks
  assets/
    icon_green.png         # connected
    icon_grey.png          # disconnected / starting
    icon_red.png           # error
```

## Config File (config.json)

```json
{
  "obs_host": "127.0.0.1",
  "obs_port": 4455,
  "obs_password": "secret",
  "osc_listen_host": "0.0.0.0",
  "osc_listen_port": 3333,
  "osc_send_host": "127.0.0.1",
  "osc_send_port": 53000,
  "log_file": "osc-obs-bridge.log"
}
```

If the file doesn't exist on startup, create it with defaults and log a message.

## Module Details

### main.rs
- Load config
- Initialise tracing (log to file + stdout)
- Create shared state via channels:
  - `osc_to_obs: mpsc::Sender<BridgeCommand>` -- OSC task sends commands to OBS task
  - `obs_to_osc: mpsc::Sender<BridgeResponse>` -- OBS task sends responses/events to OSC task
  - `status_tx: watch::Sender<AppStatus>` -- both tasks update status for tray icon
- Spawn OSC task (`tokio::spawn`)
- Spawn OBS task (`tokio::spawn`)
- Run tray icon on the main thread (required by Windows/macOS)
  - Green/grey/red icon based on `status_rx`
  - Right-click menu: "Status: Connected to OBS" / "Quit"

### config.rs
- `Config` struct with serde Deserialize + defaults
- `Config::load(path)` -- read file, fall back to defaults
- `Config::create_default(path)` -- write default config

### bridge.rs
Shared message types:

```rust
enum BridgeCommand {
    GetSceneList,
    GetActiveScene,
    SetSceneByName(String),
    SetSceneByIndex(i32),  // optional, nice-to-have
}

enum BridgeResponse {
    SceneList(Vec<String>),
    ActiveScene { index: usize, name: String },
    Error(String),
}

enum AppStatus {
    Starting,
    ObsDisconnected,
    Connected,
    Error(String),
}
```

### osc.rs
- Bind UDP socket on `osc_listen_host:osc_listen_port`
- Loop: receive UDP packet -> decode with `rosc` -> match address:
  - `/sceneList` -> send `BridgeCommand::GetSceneList`
  - `/activeScene` -> send `BridgeCommand::GetActiveScene`
  - `/scene` with string arg -> send `BridgeCommand::SetSceneByName(name)`
  - `/scene` with int arg -> send `BridgeCommand::SetSceneByIndex(n)` (optional)
  - anything else -> log and ignore
- Separate task/loop: receive from `obs_to_osc` channel -> encode OSC -> send UDP:
  - `SceneList(scenes)` -> send `/sceneListReturn` with scene names as args
  - `ActiveScene { index, name }` -> send `/activeSceneReturn` with index + name

**Important**: OSC responses must be sent back to the **sender's address** (the tablet's IP), which we get from the incoming UDP packet's source address. We also send to the configured `osc_send_host:osc_send_port` as a default.

### obs.rs
- Connect to OBS using `obws::Client::connect(host, port, password)`
- On connect: update status to `Connected`, log version info
- Loop: receive from `osc_to_obs` channel -> call OBS API:
  - `GetSceneList` -> `client.scenes().list()` -> send `SceneList` response
  - `GetActiveScene` -> `client.scenes().current_program_scene()` + list for index -> send `ActiveScene` response
  - `SetSceneByName(name)` -> `client.scenes().set_current_program_scene(&name)` -> on success, send `ActiveScene` response
  - `SetSceneByIndex(n)` -> look up name from list -> same as above
- Listen for OBS events (separate task):
  - `CurrentProgramSceneChanged` -> fetch index + name -> send `ActiveScene` response (auto-push to TouchOSC)
- **Auto-reconnect**: on disconnect, update status to `ObsDisconnected`, retry every 5 seconds

## Tray Icon Behaviour

| State | Icon | Tooltip | Menu |
|---|---|---|---|
| Starting | Grey | "osc-obs-bridge: Starting..." | Quit |
| OBS Disconnected | Grey | "osc-obs-bridge: Waiting for OBS..." | Quit |
| Connected | Green | "osc-obs-bridge: Connected (scene: X)" | Status info, Open log, Quit |
| Error | Red | "osc-obs-bridge: Error - details" | Status info, Open log, Quit |

The tray icon event loop runs on the main thread. It polls the `status_rx` watch channel to update the icon/tooltip. "Open log" opens the log file in the default text editor. "Quit" exits the process.

## Logging

Using `tracing` with two subscribers:
- **File**: append to `osc-obs-bridge.log`, INFO level, with timestamps
- **Stdout**: for debug during development

Log events:
- Startup with config summary
- OBS connected / disconnected / reconnecting
- OSC message received (DEBUG level)
- Scene change (INFO level)
- Errors (ERROR level)

## Build & Install

- `cargo build --release` produces single binary in `target/release/osc-obs-bridge.exe`
- Copy binary + config.json to install location
- Add shortcut to Windows Startup folder (`shell:startup`) for auto-start
- Future: could wrap as Windows Service, but startup folder is simpler

## Cross-compilation

```bash
# Windows (default target on Windows)
cargo build --release

# Linux
cargo build --release --target x86_64-unknown-linux-gnu

# macOS
cargo build --release --target x86_64-apple-darwin
```

## Implementation Order

1. **Scaffold**: Cargo.toml, config.rs, bridge.rs (types only)
2. **OSC listener + sender**: osc.rs with UDP socket, rosc encode/decode
3. **OBS client**: obs.rs with obws, commands + event listener + auto-reconnect
4. **Wire up**: main.rs connecting OSC <-> OBS via channels
5. **Tray icon**: main.rs tray setup with status updates
6. **Assets**: create simple coloured tray icons
7. **Test end-to-end**: with real OBS + TouchOSC
