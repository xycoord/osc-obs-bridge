use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// OBS WebSocket password — must be set by the user
    #[serde(default)]
    pub obs_password: String,

    /// OBS WebSocket host (default: 127.0.0.1)
    #[serde(default = "default_localhost")]
    pub obs_host: String,

    /// OBS WebSocket port (default: 4455)
    #[serde(default = "default_obs_port")]
    pub obs_port: u16,

    /// OSC listen host (default: "auto" — detects the machine's local network IP)
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
fn default_osc_listen_host() -> String {
    "auto".to_string()
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
            obs_password: String::new(),
            obs_host: default_localhost(),
            obs_port: default_obs_port(),
            osc_listen_host: default_osc_listen_host(), // "auto"
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

    /// Resolve the OSC listen host. If set to "auto", detects the machine's
    /// first non-loopback IPv4 address. Falls back to 127.0.0.1 if detection fails.
    pub fn resolved_osc_listen_host(&self) -> String {
        if self.osc_listen_host.eq_ignore_ascii_case("auto") {
            match detect_local_ip() {
                Some(ip) => {
                    tracing::info!("Auto-detected local IP: {ip}");
                    ip
                }
                None => {
                    tracing::warn!(
                        "Could not auto-detect local IP, falling back to 127.0.0.1"
                    );
                    "127.0.0.1".to_string()
                }
            }
        } else {
            self.osc_listen_host.clone()
        }
    }

    /// Resolve the OSC send host. If set to "broadcast", derives the broadcast
    /// address from the resolved listen host by replacing the last octet with 255.
    pub fn resolved_osc_send_host(&self, resolved_listen_host: &str) -> String {
        if self.osc_send_host.eq_ignore_ascii_case("broadcast") {
            match resolved_listen_host.rfind('.') {
                Some(pos) => format!("{}.255", &resolved_listen_host[..pos]),
                None => {
                    tracing::warn!(
                        "Cannot derive broadcast from '{resolved_listen_host}', falling back to 255.255.255.255"
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

/// Detect the machine's first non-loopback IPv4 address.
fn detect_local_ip() -> Option<String> {
    if_addrs::get_if_addrs()
        .ok()?
        .into_iter()
        .find(|iface| {
            !iface.is_loopback() && iface.addr.ip().is_ipv4()
        })
        .map(|iface| iface.addr.ip().to_string())
}
