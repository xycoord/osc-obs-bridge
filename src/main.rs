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
    info!(
        "OBS: {}:{} | OSC listen: {}:{} | OSC send: {}:{}",
        config.obs_host,
        config.obs_port,
        config.osc_listen_host,
        config.osc_listen_port,
        config.osc_send_host,
        config.osc_send_port,
    );

    // Channels
    let (cmd_tx, cmd_rx) = mpsc::channel::<bridge::BridgeCommand>(64);
    let (resp_tx, resp_rx) = mpsc::channel::<bridge::BridgeResponse>(64);
    let (status_tx, status_rx) = watch::channel(AppStatus::Starting);

    // Build the tray icon on the main thread (required by Windows/macOS)
    let icon_green = load_embedded_icon(IconColor::Green);
    let icon_grey = load_embedded_icon(IconColor::Grey);
    let icon_red = load_embedded_icon(IconColor::Red);

    // Menu items
    let status_item = MenuItem::new("osc-obs-bridge: Starting...", false, None);
    let separator = PredefinedMenuItem::separator();
    let open_log_item = MenuItem::new("Open Log File", true, None);
    let quit_item = MenuItem::new("Quit", true, None);

    let menu = Menu::new();
    menu.append(&status_item)?;
    menu.append(&separator)?;
    menu.append(&open_log_item)?;
    menu.append(&quit_item)?;

    let tray = TrayIconBuilder::new()
        .with_tooltip("osc-obs-bridge: Starting...")
        .with_icon(icon_grey.clone())
        .with_menu(Box::new(menu))
        .build()?;

    // Start the tokio runtime in a background thread
    let osc_config = config.clone();
    let obs_config = config.clone();
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        rt.block_on(async {
            // Spawn OSC task
            let osc_status_tx = status_tx.clone();
            let osc_handle = tokio::spawn(async move {
                if let Err(e) = osc::run(osc_config, cmd_tx, resp_rx, osc_status_tx).await {
                    error!("OSC task failed: {e}");
                }
            });

            // Spawn OBS task
            let obs_status_tx = status_tx.clone();
            let obs_handle = tokio::spawn(async move {
                obs::run(obs_config, cmd_rx, resp_tx, obs_status_tx).await;
            });

            // Wait for either task to complete (they shouldn't unless there's an error)
            tokio::select! {
                r = osc_handle => {
                    error!("OSC task ended: {r:?}");
                    running_clone.store(false, Ordering::Relaxed);
                }
                r = obs_handle => {
                    error!("OBS task ended: {r:?}");
                    running_clone.store(false, Ordering::Relaxed);
                }
            }
        });
    });

    // Main thread: pump the Win32/GTK event loop and update the tray icon.
    // We use a simple polling loop since we don't have a full windowing toolkit.
    let menu_rx = muda::MenuEvent::receiver();
    let quit_id = quit_item.id().clone();
    let open_log_id = open_log_item.id().clone();

    let mut last_status = String::new();

    loop {
        // Process menu events
        if let Ok(event) = menu_rx.try_recv() {
            if event.id == quit_id {
                info!("Quit requested from tray menu");
                break;
            } else if event.id == open_log_id {
                let _ = open::that(&log_path);
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
        // On Windows this processes Win32 messages; on Linux it would process GTK events
        #[cfg(target_os = "windows")]
        unsafe {
            use winapi::um::winuser::{DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE};
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(33));
    }

    info!("osc-obs-bridge shutting down");
    Ok(())
}

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
