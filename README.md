# OSC OBS Bridge

A lightweight bridge that lets you control OBS Studio scene switching from TouchOSC (or any OSC controller) over the network. It runs silently in the system tray and uses almost no resources.

**Windows only** for now — Linux and macOS builds on request.

## Requirements

- **OBS Studio** 28+ with the built-in WebSocket server enabled (obs-websocket v5)
- **Windows** 10 or later

## Getting Started

### 1. Install

Download `osc-obs-bridge.exe` from the [latest release](https://github.com/xycoord/osc-obs-bridge/releases) and put it in a folder (e.g. `C:\Program Files\osc-obs-bridge\`).

### 2. First Run

Run the exe. Three things happen:
- **Windows Firewall** will ask to allow network access — click **Allow**. The bridge needs this to receive OSC messages from your tablet.
- A `config.json` file is created next to the exe with default settings
- A dot ⚪ appears in your system tray (bottom-right of the taskbar — you may need to click the `^` arrow to see it)

### 3. Configure

Right-click the tray dot and choose **Open Config**. This opens `config.json` in your text editor. You need to change two things:

**Your OBS WebSocket password** — find this in OBS under Tools > WebSocket Server Settings. Copy the password and put it in the config:
```json
"obs_password": "your-password-here"
```

**Your machine's IP** — run `ipconfig` in a command prompt and find your local IP (e.g. `192.168.1.50`):
```json
"osc_listen_host": "192.168.1.50"
```

The bridge will automatically broadcast responses to all devices on your network (it derives the broadcast address from your IP — e.g. `192.168.1.50` becomes `192.168.1.255`).

> **Tip:** If you'd rather target a single tablet, set `osc_send_host` to the tablet's IP (find it in the tablet's Wi-Fi settings) instead of the default `"broadcast"`.

⚠️ The machine running the bridge should have a **static IP** (set in Windows network settings or reserved in your router's DHCP settings). If the IP changes, both `osc_listen_host` and your TouchOSC connection will stop working.

⚠️ The default ports (9000/8000) match TouchOSC's defaults. If you've changed them in TouchOSC, update the config to match. 

### 4. Reload

Save the config file, then right-click the tray dot and choose **Reload Config**. 
If everything is set up correctly, the dot turns green 🟢.

### 5. Start on Boot (optional)

Press `Win+R`, type `shell:startup`, and put a shortcut to the exe in that folder.

## System Tray

The tray dot shows connection status at a glance:

| Icon | Meaning |
|---|---|
| 🟢 | Connected to OBS |
| ⚪ | Starting up, or OBS is disconnected (auto-reconnects every 5s) |
| 🔴 | Error |

Right-click menu:
- **Status line** — current state and active scene name
- **Open Config** — opens `config.json` in your text editor
- **Reload Config** — applies config changes without restarting
- **Open Log File** — opens the log for debugging
- **Quit** — exits the bridge

## Config Reference

| Setting | What to set it to |
|---|---|
| `obs_host` | IP of the machine running OBS. Leave as `127.0.0.1` if OBS runs on the same machine as the bridge |
| `obs_port` | Must match the port in OBS > Tools > WebSocket Server Settings (default 4455) |
| `obs_password` | **Must change.** Copy from OBS > Tools > WebSocket Server Settings |
| `osc_listen_host` | **Must change.** The bridge machine's local IP (e.g. `192.168.1.50`) |
| `osc_listen_port` | Must match TouchOSC's **send port** (default 9000) |
| `osc_send_host` | Default `"broadcast"` — auto-derives from `osc_listen_host`. Or set a specific tablet's IP |
| `osc_send_port` | Must match TouchOSC's **receive port** (default 8000) |
| `log_file` | Where to write logs (default `osc-obs-bridge.log` next to the exe) |

## Troubleshooting

**Tray dot stays grey**
- Is OBS running with the WebSocket server enabled? (Tools > WebSocket Server Settings)
- Does `obs_password` in your config match exactly? (case-sensitive)
- If OBS is on a different machine, check that `obs_host` is set to that machine's IP and its firewall allows TCP on the WebSocket port

**TouchOSC doesn't receive responses**
- Is `osc_send_host` set to your tablet's IP? (not `127.0.0.1`)
- Do `osc_listen_port` and `osc_send_port` match the ports in your TouchOSC connection settings? (note: TouchOSC's "send port" is the bridge's "listen port" and vice versa)
- Are the tablet and bridge machine on the same network?

**Scene list doesn't update after changes in OBS**
- The bridge auto-detects scene changes via OBS events and polls every 1 second for reordering. If it's not updating, check the log file for errors

**Config changes not taking effect**
- Use **Reload Config** from the tray menu — editing the file alone doesn't apply changes
- Check the log file for config parse errors (missing comma, trailing comma, etc.)

---

## OSC Protocol Reference

This section is for anyone building or modifying a TouchOSC layout (or any OSC controller) to work with this bridge.

### Inbound Messages (Controller -> Bridge)

These are OSC messages the bridge listens for:

#### `/sceneList`

Request the list of all scenes in OBS.

- **Arguments:** none
- **Response:** `/sceneListReturn` (see below)

#### `/activeScene`

Request which scene is currently active.

- **Arguments:** none
- **Response:** `/activeSceneReturn` (see below)

#### `/scene`

Switch to a scene.

- **Arguments:** scene name (string) OR scene index (integer, 1-based)
- **Response:** The bridge sends `/activeSceneReturn` automatically via the scene change event
- **On error:** If the scene name doesn't exist or the index is out of range, the bridge sends back `/sceneListReturn` with the current list so the controller can resync

Examples:
```
/scene "Band Logo"     -- switch by name
/scene 3               -- switch to 3rd scene (1-based)
```

### Outbound Messages (Bridge -> Controller)

These are OSC messages the bridge sends back:

#### `/sceneListReturn`

The full list of scene names.

- **Arguments:** string, string, string, ... (one per scene, in OBS order)
- **Sent in response to:** `/sceneList`, or automatically when scenes are added/removed/renamed/reordered in OBS

Example:
```
/sceneListReturn "Band Logo" "Intermission" "Camera 1" "Camera 2"
```

#### `/activeSceneReturn`

The currently active scene.

- **Arguments:** index (int, 0-based), name (string)
- **Sent in response to:** `/activeScene`, or automatically whenever the active scene changes in OBS (from any source — TouchOSC, OBS itself, hotkeys, etc.)

Example:
```
/activeSceneReturn 2 "Camera 1"
```

### Auto-Push Behaviour

The bridge proactively pushes updates without being asked:

| OBS Event | What's Pushed |
|---|---|
| Active scene changes | `/activeSceneReturn` |
| Scene created | `/sceneListReturn` |
| Scene removed | `/sceneListReturn` |
| Scene renamed | `/sceneListReturn` |
| Scene list reordered | `/sceneListReturn` (detected by 1s polling) |

This keeps all connected controllers in sync even when changes happen directly in OBS.

### TouchOSC Integration Example

A typical TouchOSC Lua script for a radio button scene switcher:

```lua
function init()
    sendOSC("/sceneList")
end

function onReceiveOSC(message, connections)
    local path = message[1]
    if path == "/sceneListReturn" then
        -- message[2] is an array of {value = "scene name"} tables
        -- Update your radio button steps and labels here
        sendOSC("/activeScene")
    elseif path == "/activeSceneReturn" then
        -- message[2][1].value is the 0-based index
        -- Set your radio button value here
    end
end

function onValueChanged(key)
    if key == "x" then
        local sceneName = scenesList[self.values["x"] + 1]
        sendOSC("/scene", sceneName)
    end
end
```

## Development

See [CLAUDE.md](CLAUDE.md) for architecture, build instructions, and developer documentation.
