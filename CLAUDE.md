# CLAUDE.md — Developer Guide for osc-obs-bridge

## What This Is

A lightweight Rust bridge between OSC (Open Sound Control) and OBS Studio's WebSocket v5 API. It translates a small set of OSC messages into OBS API calls for scene switching, and pushes OBS state changes back as OSC messages. Designed to run as a silent tray app at a music venue.

## Architecture

```
TouchOSC (tablet)           osc-obs-bridge              OBS Studio
    UDP:9000 ──────> [osc.rs] ──mpsc──> [obs.rs] ──WebSocket──> OBS
    UDP:8000 <────── [osc.rs] <──mpsc── [obs.rs] <──events──── OBS
                                  |
                            watch::channel
                                  |
                          [main.rs] tray icon
```

Three concurrent components communicate via tokio channels:
- **osc.rs** — UDP socket for OSC recv/send, rosc encode/decode
- **obs.rs** — obws WebSocket client, command handling, event listening, 1s scene list poll
- **main.rs** — tray icon on main thread (required by Windows/macOS), reload orchestration

The main thread runs a Win32 message pump at ~30Hz. The tokio runtime runs in a background thread.

## Key Design Decisions

**Why tokio channels instead of direct calls?** The OSC and OBS tasks need to be independently restartable (for config reload). Channels decouple them so we can abort and respawn without shared state.

**Why poll the scene list?** OBS WebSocket v5 (tested v5.6.3) does not fire any event when scenes are reordered. We poll every 1 second and diff against a cached list. Events (SceneCreated, SceneRemoved, SceneNameChanged) handle everything else and also update the poll cache to prevent duplicate pushes.

**Why track the client IP?** TouchOSC may send from a different IP than the configured `osc_send_host`. We remember the IP of the last inbound OSC message and send responses to both the configured address and the detected client (using the configured port, not the ephemeral source port).

**Why not use the `image` crate for tray icons?** Icons are 32x32 coloured circles generated programmatically in `load_embedded_icon()`. No asset files needed, no image decoding dependency.

## File Structure

```
src/
  main.rs       Entry point, tray icon, event loop, reload orchestration
  config.rs     JSON config with serde defaults, load-or-create logic
  bridge.rs     Shared types: BridgeCommand, BridgeResponse, AppStatus
  osc.rs        OSC UDP listener + sender, rosc packet handling
  obs.rs        OBS WebSocket client, command dispatch, event handling, polling
```

## Build & Run

```bash
cargo build --release    # produces target/release/osc-obs-bridge.exe (~3.2MB)
cargo run                # debug build with console window visible
```

## Config Reload Flow

1. User clicks "Reload Config" in tray menu
2. Main thread sends `()` on `reload_tx` (mpsc, capacity 1)
3. `run_bridge_loop` receives signal, aborts both task handles
4. 500ms delay for socket teardown (the orphaned OSC sender task exits when `resp_tx` is dropped)
5. Config re-read from disk
6. Fresh channels created, new tasks spawned with new config

## OSC Protocol

### Inbound (OSC -> Bridge)

| Address | Args | Bridge Action |
|---|---|---|
| `/sceneList` | (none) | `GetSceneList` -> fetch + respond |
| `/activeScene` | (none) | `GetActiveScene` -> fetch + respond |
| `/scene` | string name | `SetSceneByName` -> switch scene |
| `/scene` | int index (1-based) | `SetSceneByIndex` -> look up + switch |

### Outbound (Bridge -> OSC)

| Address | Args | Trigger |
|---|---|---|
| `/sceneListReturn` | string, string, ... | On request, or auto-push on scene list change |
| `/activeSceneReturn` | int (0-based index), string (name) | On request, or auto-push on scene change |

## OBS Events Handled

| Event | Action |
|---|---|
| `CurrentProgramSceneChanged` | Push `/activeSceneReturn` |
| `SceneCreated` | Push `/sceneListReturn` + update cache |
| `SceneRemoved` | Push `/sceneListReturn` + update cache |
| `SceneNameChanged` | Push `/sceneListReturn` + update cache |
| `SceneListChanged` | Push `/sceneListReturn` + update cache |
| (1s poll diff) | Push `/sceneListReturn` if list changed (catches reordering) |

## Known Limitations & Gotchas

- **`is_connection_error()` is string-matching**: Checks the Debug output of anyhow errors for keywords like "disconnect", "connection", "closed". Fragile if obws changes error messages, but there's no better way without obws exposing typed error variants.

- **Socket rebind race on reload**: The orphaned OSC sender task holds an `Arc<UdpSocket>` until `resp_rx` closes. The 500ms delay in the reload loop covers this in practice, but technically the new bind could fail if the sender is blocked in `send_to` for >500ms. Very unlikely with UDP to local IPs.

- **Linux/macOS tray icon**: `pump_event_loop()` has stub implementations that log a warning. Full support needs GTK (Linux) or Cocoa (macOS) event loop integration.

- **Log level**: Default is INFO. Debug messages exist in both osc.rs and obs.rs but are filtered at runtime. To see them, add `.with_max_level(tracing::Level::DEBUG)` to the subscriber init in main.rs.

- **No graceful WebSocket close**: On quit, the process exits and the tokio thread is killed. OBS handles this fine (treats it as a disconnect).

## Dependencies

| Crate | Purpose |
|---|---|
| `tokio` | Async runtime (full features) |
| `obws` 0.15 | OBS WebSocket v5 client with typed API + events |
| `rosc` 0.11 | OSC 1.0 message codec (we handle UDP ourselves) |
| `tray-icon` + `muda` | Cross-platform system tray icon + context menu |
| `serde` + `serde_json` | Config serialization |
| `tracing` + `tracing-subscriber` + `tracing-appender` | Structured logging to file |
| `anyhow` | Error handling |
| `futures-util` | `StreamExt` for OBS event stream iteration |
| `open` | "Open Config" / "Open Log" opens files in default app |
| `winapi` (Windows only) | Win32 message pump for tray icon |
