use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// OBS WebSocket host (default: 127.0.0.1)
    #[serde(default = "default_localhost")]
    pub obs_host: String,

    /// OBS WebSocket port (default: 4455)
    #[serde(default = "default_obs_port")]
    pub obs_port: u16,

    /// OBS WebSocket password (default: "secret")
    #[serde(default = "default_obs_password")]
    pub obs_password: String,

    /// OSC listen host (default: 0.0.0.0 to accept from network)
    #[serde(default = "default_osc_listen_host")]
    pub osc_listen_host: String,

    /// OSC listen port (default: 3333)
    #[serde(default = "default_osc_listen_port")]
    pub osc_listen_port: u16,

    /// OSC send host for responses (default: "broadcast" — derives broadcast address from osc_listen_host)
    #[serde(default = "default_osc_send_host")]
    pub osc_send_host: String,

    /// OSC send port for responses (default: 53000)
    #[serde(default = "default_osc_send_port")]
    pub osc_send_port: u16,

    /// Log file path (default: osc-obs-bridge.log next to binary)
    #[serde(default = "default_log_file")]
    pub log_file: String,
}

fn default_localhost() -> String {
    "127.0.0.1".to_string()
}
fn default_obs_port() -> u16 {
    4455
}
fn default_obs_password() -> String {
    "secret".to_string()
}
fn default_osc_listen_host() -> String {
    "0.0.0.0".to_string()
}
fn default_osc_listen_port() -> u16 {
    9000
}
fn default_osc_send_host() -> String {
    "broadcast".to_string()
}
fn default_osc_send_port() -> u16 {
    8000
}
fn default_log_file() -> String {
    "osc-obs-bridge.log".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            obs_host: default_localhost(),
            obs_port: default_obs_port(),
            obs_password: default_obs_password(),
            osc_listen_host: default_osc_listen_host(),
            osc_listen_port: default_osc_listen_port(),
            osc_send_host: default_osc_send_host(),
            osc_send_port: default_osc_send_port(),
            log_file: default_log_file(),
        }
    }
}

impl Config {
    /// Load config from a file path, or create a default config file if it doesn't exist.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        if path.exists() {
            let contents =
                std::fs::read_to_string(path).context("Failed to read config file")?;
            let config: Config =
                serde_json::from_str(&contents).context("Failed to parse config file")?;
            Ok(config)
        } else {
            let config = Config::default();
            let contents = serde_json::to_string_pretty(&config)
                .context("Failed to serialize default config")?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::write(path, contents).context("Failed to write default config file")?;
            Ok(config)
        }
    }

    /// Resolve the OSC send host. If set to "broadcast", derives the broadcast
    /// address from `osc_listen_host` by replacing the last octet with 255.
    pub fn resolved_osc_send_host(&self) -> String {
        if self.osc_send_host.eq_ignore_ascii_case("broadcast") {
            match self.osc_listen_host.rfind('.') {
                Some(pos) => format!("{}.255", &self.osc_listen_host[..pos]),
                None => {
                    tracing::warn!(
                        "Cannot derive broadcast from '{}', falling back to 255.255.255.255",
                        self.osc_listen_host
                    );
                    "255.255.255.255".to_string()
                }
            }
        } else {
            self.osc_send_host.clone()
        }
    }

    /// Resolve the config file path: same directory as the running executable.
    pub fn default_path() -> PathBuf {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("config.json")))
            .unwrap_or_else(|| PathBuf::from("config.json"))
    }
}
