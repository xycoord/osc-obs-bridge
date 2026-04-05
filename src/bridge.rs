/// Shared message types between the OSC and OBS tasks.

/// Commands sent from the OSC listener to the OBS client.
#[derive(Debug)]
pub enum BridgeCommand {
    /// Request the list of all scene names.
    GetSceneList,
    /// Request the currently active scene (index + name).
    GetActiveScene,
    /// Switch to a scene by name.
    SetSceneByName(String),
    /// Switch to a scene by 1-based index.
    SetSceneByIndex(i32),
}

/// Responses sent from the OBS client back to the OSC sender.
#[derive(Debug, Clone)]
pub enum BridgeResponse {
    /// Full list of scene names.
    SceneList(Vec<String>),
    /// Currently active scene: 0-based index and name.
    ActiveScene { index: usize, name: String },
    /// Bridge status string for diagnostics.
    Status(String),
}

/// Application status, watched by the tray icon.
#[derive(Debug, Clone)]
pub enum AppStatus {
    Starting,
    OscListening,
    ObsDisconnected,
    Connected { scene: String },
    Error(String),
}

impl AppStatus {
    /// Convert to a short status string for the `/bridgeStatus` OSC message.
    pub fn osc_status(&self) -> &str {
        match self {
            AppStatus::Starting => "starting",
            AppStatus::OscListening => "obs_disconnected",
            AppStatus::ObsDisconnected => "obs_disconnected",
            AppStatus::Connected { .. } => "connected",
            AppStatus::Error(e) if e.contains("password not set") => "obs_password_not_set",
            AppStatus::Error(e) if e.contains("password incorrect") => "obs_password_incorrect",
            AppStatus::Error(_) => "error",
        }
    }
}

impl std::fmt::Display for AppStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppStatus::Starting => write!(f, "Starting..."),
            AppStatus::OscListening => write!(f, "OSC listening, waiting for OBS..."),
            AppStatus::ObsDisconnected => write!(f, "OBS disconnected, reconnecting..."),
            AppStatus::Connected { scene } => write!(f, "Connected — Scene: {scene}"),
            AppStatus::Error(e) => write!(f, "Error: {e}"),
        }
    }
}
