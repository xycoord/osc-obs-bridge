// Hide the console window on Windows release builds.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod bridge;
mod config;
mod obs;
mod osc;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use muda::{Menu, MenuItem, PredefinedMenuItem};
use tokio::sync::{mpsc, watch};
use tray_icon::TrayIconBuilder;
use tracing::{error, info};

use bridge::AppStatus;

fn main() -> Result<()> {
    // Load config
    let config_path = config::Config::default_path();
    let config = config::Config::load_or_create(&config_path)?;

    // Set up logging: file + stdout
    let log_path = if std::path::Path::new(&config.log_file).is_absolute() {
        std::path::PathBuf::from(&config.log_file)
    } else {
        config_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join(&config.log_file)
    };

    let log_dir = log_path.parent().unwrap_or(std::path::Path::new("."));
    let log_filename = log_path
        .file_name()
        .unwrap_or(std::ffi::OsStr::new("osc-obs-bridge.log"));

    let file_appender = tracing_appender::rolling::never(log_dir, log_filename);
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(false)
        .init();

    info!("osc-obs-bridge starting");
    info!("Config loaded from: {}", config_path.display());
    log_config(&config);

    // Status channel (watched by tray icon on main thread)
    let (status_tx, status_rx) = watch::channel(AppStatus::Starting);

    // Reload signal: main thread sends () to tell the runtime to restart tasks
    let (reload_tx, reload_rx) = mpsc::channel::<()>(1);

    // Build the tray icon on the main thread (required by Windows/macOS)
    let icon_green = load_embedded_icon(IconColor::Green);
    let icon_grey = load_embedded_icon(IconColor::Grey);
    let icon_red = load_embedded_icon(IconColor::Red);

    // Menu items
    let status_item = MenuItem::new("osc-obs-bridge: Starting...", false, None);
    let separator1 = PredefinedMenuItem::separator();
    let open_config_item = MenuItem::new("Open Config", true, None);
    let reload_config_item = MenuItem::new("Reload Config", true, None);
    let open_log_item = MenuItem::new("Open Log File", true, None);
    let separator2 = PredefinedMenuItem::separator();
    let quit_item = MenuItem::new("Quit", true, None);

    let menu = Menu::new();
    menu.append(&status_item)?;
    menu.append(&separator1)?;
    menu.append(&open_config_item)?;
    menu.append(&reload_config_item)?;
    menu.append(&open_log_item)?;
    menu.append(&separator2)?;
    menu.append(&quit_item)?;

    let tray = TrayIconBuilder::new()
        .with_tooltip("osc-obs-bridge: Starting...")
        .with_icon(icon_grey.clone())
        .with_menu(Box::new(menu))
        .build()?;

    // Start the tokio runtime in a background thread.
    // It runs a loop: spawn tasks, wait for reload signal or task failure, then restart.
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();
    let runtime_config_path = config_path.clone();
    let initial_config = config;
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        rt.block_on(async {
            run_bridge_loop(
                initial_config,
                runtime_config_path,
                reload_rx,
                status_tx,
                running_clone,
            )
            .await;
        });
    });

    // Main thread: pump the Win32/GTK event loop and update the tray icon.
    let menu_rx = muda::MenuEvent::receiver();
    let quit_id = quit_item.id().clone();
    let open_log_id = open_log_item.id().clone();
    let open_config_id = open_config_item.id().clone();
    let reload_config_id = reload_config_item.id().clone();

    let mut last_status = String::new();

    loop {
        // Process menu events
        if let Ok(event) = menu_rx.try_recv() {
            if event.id == quit_id {
                info!("Quit requested from tray menu");
                break;
            } else if event.id == open_log_id {
                let _ = open::that(&log_path);
            } else if event.id == open_config_id {
                info!("Opening config file: {}", config_path.display());
                let _ = open::that(&config_path);
            } else if event.id == reload_config_id {
                info!("Reload config requested from tray menu");
                let _ = reload_tx.try_send(());
            }
        }

        // Update tray icon based on status
        let status = status_rx.borrow().clone();
        let status_str = status.to_string();

        if status_str != last_status {
            let icon = match &status {
                AppStatus::Connected { .. } => &icon_green,
                AppStatus::Error(_) => &icon_red,
                _ => &icon_grey,
            };

            let tooltip = format!("osc-obs-bridge: {status_str}");
            let _ = tray.set_icon(Some(icon.clone()));
            let _ = tray.set_tooltip(Some(&tooltip));
            let _ = status_item.set_text(&tooltip);

            last_status = status_str;
        }

        // Check if runtime is still alive
        if !running.load(Ordering::Relaxed) {
            error!("Runtime stopped unexpectedly");
            break;
        }

        // Pump the native event loop at ~30Hz
        pump_event_loop();

        std::thread::sleep(std::time::Duration::from_millis(33));
    }

    info!("osc-obs-bridge shutting down");
    Ok(())
}

/// The async bridge loop that runs in the tokio runtime thread.
/// Spawns OSC + OBS tasks, and restarts them when a reload signal is received.
async fn run_bridge_loop(
    initial_config: config::Config,
    config_path: std::path::PathBuf,
    mut reload_rx: mpsc::Receiver<()>,
    status_tx: watch::Sender<AppStatus>,
    running: Arc<AtomicBool>,
) {
    let mut current_config = initial_config;

    loop {
        let _ = status_tx.send(AppStatus::Starting);

        // Create fresh channels for this run
        let (cmd_tx, cmd_rx) = mpsc::channel::<bridge::BridgeCommand>(64);
        let (resp_tx, resp_rx) = mpsc::channel::<bridge::BridgeResponse>(64);

        // Spawn tasks
        let osc_config = current_config.clone();
        let osc_status_tx = status_tx.clone();
        let mut osc_handle = tokio::spawn(async move {
            if let Err(e) = osc::run(osc_config, cmd_tx, resp_rx, osc_status_tx).await {
                error!("OSC task failed: {e}");
            }
        });

        let obs_config = current_config.clone();
        let obs_status_tx = status_tx.clone();
        let mut obs_handle = tokio::spawn(async move {
            obs::run(obs_config, cmd_rx, resp_tx, obs_status_tx).await;
        });

        // Wait for either: a reload signal, or a task to exit unexpectedly
        tokio::select! {
            _ = reload_rx.recv() => {
                info!("Reload signal received, restarting tasks...");
            }
            r = &mut osc_handle => {
                error!("OSC task ended unexpectedly: {r:?}");
                running.store(false, Ordering::Relaxed);
                return;
            }
            r = &mut obs_handle => {
                error!("OBS task ended unexpectedly: {r:?}");
                running.store(false, Ordering::Relaxed);
                return;
            }
        }

        // Abort running tasks before restarting
        osc_handle.abort();
        obs_handle.abort();

        // Small delay to let sockets close
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Reload config from disk
        match config::Config::load_or_create(&config_path) {
            Ok(new_config) => {
                info!("Config reloaded successfully");
                log_config(&new_config);
                current_config = new_config;
            }
            Err(e) => {
                error!("Failed to reload config, keeping current settings: {e}");
            }
        }
    }
}

fn log_config(config: &config::Config) {
    info!(
        "OBS: {}:{} | OSC listen: {}:{} | OSC send: {}:{}",
        config.obs_host,
        config.obs_port,
        config.osc_listen_host,
        config.osc_listen_port,
        config.osc_send_host,
        config.osc_send_port,
    );
}

// --- Platform-specific event loop pumping ---

/// Pump the native event loop so the tray icon and menus work.
#[cfg(target_os = "windows")]
fn pump_event_loop() {
    unsafe {
        use winapi::um::winuser::{
            DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
        };
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// On Linux, tray-icon requires a GTK event loop. This is a stub that logs a warning once.
/// Full Linux support would require the `gtk` crate and `gtk::main_iteration_do(false)`.
#[cfg(target_os = "linux")]
fn pump_event_loop() {
    use std::sync::Once;
    static WARN: Once = Once::new();
    WARN.call_once(|| {
        tracing::warn!("Linux tray icon support requires a GTK event loop — tray icon may not work");
    });
}

/// On macOS, tray-icon requires a Cocoa run loop on the main thread.
/// Full macOS support would require `objc` / `cocoa` crates.
#[cfg(target_os = "macos")]
fn pump_event_loop() {
    use std::sync::Once;
    static WARN: Once = Once::new();
    WARN.call_once(|| {
        tracing::warn!("macOS tray icon support requires a Cocoa run loop — tray icon may not work");
    });
}

/// Fallback for other platforms.
#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
fn pump_event_loop() {}

// --- Embedded tray icons (simple colored circles) ---

#[derive(Clone, Copy)]
enum IconColor {
    Green,
    Grey,
    Red,
}

/// Generate a simple 32x32 colored circle icon as an RGBA image.
fn load_embedded_icon(color: IconColor) -> tray_icon::Icon {
    let size: u32 = 32;
    let center = size as f32 / 2.0;
    let radius = center - 2.0;

    let (r, g, b) = match color {
        IconColor::Green => (0x2E, 0xCC, 0x40),
        IconColor::Grey => (0x99, 0x99, 0x99),
        IconColor::Red => (0xE7, 0x4C, 0x3C),
    };

    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let dist = (dx * dx + dy * dy).sqrt();

            if dist <= radius {
                // Inside the circle: smooth edge with anti-aliasing
                let alpha = if dist > radius - 1.0 {
                    ((radius - dist) * 255.0) as u8
                } else {
                    255
                };
                rgba.extend_from_slice(&[r, g, b, alpha]);
            } else {
                // Outside: transparent
                rgba.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }

    tray_icon::Icon::from_rgba(rgba, size, size).expect("Failed to create tray icon")
}
