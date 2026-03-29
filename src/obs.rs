use anyhow::Result;
use futures_util::StreamExt;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, warn};

use crate::bridge::{AppStatus, BridgeCommand, BridgeResponse};
use crate::config::Config;

/// Run the OBS WebSocket client task.
///
/// Connects to OBS, handles commands from the OSC layer, and listens for OBS events.
/// Automatically reconnects on disconnect.
pub async fn run(
    config: Config,
    mut cmd_rx: mpsc::Receiver<BridgeCommand>,
    resp_tx: mpsc::Sender<BridgeResponse>,
    status_tx: watch::Sender<AppStatus>,
) {
    loop {
        info!(
            "Connecting to OBS at {}:{}...",
            config.obs_host, config.obs_port
        );

        match connect_and_run(&config, &mut cmd_rx, &resp_tx, &status_tx).await {
            Ok(()) => {
                info!("OBS connection closed cleanly");
            }
            Err(e) => {
                warn!("OBS connection error: {e}");
            }
        }

        let _ = status_tx.send(AppStatus::ObsDisconnected);
        info!("Reconnecting to OBS in 5 seconds...");
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

/// Fetch the scene list and find the index of a given scene name.
async fn get_scene_index(
    client: &obws::Client,
    scene_name: &str,
) -> Result<(usize, Vec<String>)> {
    let scenes = client.scenes().list().await?;
    let names: Vec<String> = scenes.scenes.iter().map(|s| s.id.name.clone()).collect();
    let index = names.iter().position(|n| n == scene_name).unwrap_or(0);
    Ok((index, names))
}

/// Connect to OBS and process commands/events until disconnection.
async fn connect_and_run(
    config: &Config,
    cmd_rx: &mut mpsc::Receiver<BridgeCommand>,
    resp_tx: &mpsc::Sender<BridgeResponse>,
    status_tx: &watch::Sender<AppStatus>,
) -> Result<()> {
    let client = obws::Client::connect(
        &config.obs_host,
        config.obs_port,
        Some(&config.obs_password),
    )
    .await?;

    let version = client.general().version().await?;
    info!(
        "Connected to OBS (WebSocket v{}, RPC v{})",
        version.obs_web_socket_version, version.rpc_version
    );

    // Get current scene for status
    let current = client.scenes().current_program_scene().await?;
    let _ = status_tx.send(AppStatus::Connected {
        scene: current.id.name.clone(),
    });

    // Set up event listener for scene changes.
    let mut events = client.events()?;

    // Poll timer: check for scene list changes every second.
    // OBS doesn't fire events for scene reordering, so we poll and diff.
    let mut poll_interval = tokio::time::interval(std::time::Duration::from_secs(1));
    poll_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut cached_scene_list: Vec<String> = Vec::new();

    // Main loop: process commands, events, and poll concurrently
    loop {
        tokio::select! {
            // Handle commands from OSC
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(cmd) => {
                        if let Err(e) = handle_command(&client, cmd, resp_tx, status_tx).await {
                            error!("Error handling command: {e}");
                            if is_connection_error(&e) {
                                break;
                            }
                        }
                    }
                    None => {
                        // Channel closed, shutting down
                        break;
                    }
                }
            }

            // Handle OBS events
            event = events.next() => {
                match event {
                    Some(obws::events::Event::CurrentProgramSceneChanged { id }) => {
                        info!("OBS scene changed: {}", id.name);
                        let _ = status_tx.send(AppStatus::Connected {
                            scene: id.name.clone(),
                        });

                        // Look up the index and send /activeSceneReturn
                        match get_scene_index(&client, &id.name).await {
                            Ok((index, _names)) => {
                                let _ = resp_tx
                                    .send(BridgeResponse::ActiveScene {
                                        index,
                                        name: id.name,
                                    })
                                    .await;
                            }
                            Err(e) => {
                                warn!("Failed to get scene index: {e}");
                                let _ = resp_tx
                                    .send(BridgeResponse::ActiveScene {
                                        index: 0,
                                        name: id.name,
                                    })
                                    .await;
                            }
                        }
                    }
                    // Scene list structure changed — push updated list to TouchOSC
                    Some(obws::events::Event::SceneCreated { id, .. }) => {
                        info!("OBS scene created: {}", id.name);
                        push_scene_list(&client, resp_tx, &mut cached_scene_list).await;
                    }
                    Some(obws::events::Event::SceneRemoved { id, .. }) => {
                        info!("OBS scene removed: {}", id.name);
                        push_scene_list(&client, resp_tx, &mut cached_scene_list).await;
                    }
                    Some(obws::events::Event::SceneNameChanged { old_name, new_name, .. }) => {
                        info!("OBS scene renamed: {old_name} -> {new_name}");
                        push_scene_list(&client, resp_tx, &mut cached_scene_list).await;
                    }
                    Some(obws::events::Event::SceneListChanged { .. }) => {
                        info!("OBS scene list changed");
                        push_scene_list(&client, resp_tx, &mut cached_scene_list).await;
                    }
                    Some(other) => {
                        debug!("Ignoring OBS event: {other:?}");
                    }
                    None => {
                        // Event stream ended, OBS probably disconnected
                        warn!("OBS event stream ended");
                        break;
                    }
                }
            }

            // Poll for scene list changes (catches reordering, which OBS has no event for)
            _ = poll_interval.tick() => {
                match client.scenes().list().await {
                    Ok(scenes) => {
                        let names: Vec<String> =
                            scenes.scenes.iter().map(|s| s.id.name.clone()).collect();
                        if names != cached_scene_list {
                            if !cached_scene_list.is_empty() {
                                info!("Scene list changed (detected by poll): {} scenes", names.len());
                                let _ = resp_tx
                                    .send(BridgeResponse::SceneList(names.clone()))
                                    .await;
                            }
                            cached_scene_list = names;
                        }
                    }
                    Err(e) => {
                        let ae: anyhow::Error = e.into();
                        if is_connection_error(&ae) {
                            warn!("Poll failed (connection error): {ae}");
                            break;
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Handle a single bridge command by calling the OBS API.
async fn handle_command(
    client: &obws::Client,
    cmd: BridgeCommand,
    resp_tx: &mpsc::Sender<BridgeResponse>,
    status_tx: &watch::Sender<AppStatus>,
) -> Result<()> {
    match cmd {
        BridgeCommand::GetSceneList => {
            let scenes = client.scenes().list().await?;
            let names: Vec<String> = scenes.scenes.iter().map(|s| s.id.name.clone()).collect();
            info!("Scene list requested: {} scenes", names.len());
            resp_tx.send(BridgeResponse::SceneList(names)).await?;
        }

        BridgeCommand::GetActiveScene => {
            // Use a single list() call which includes the current scene,
            // avoiding a race between separate list + current_program_scene calls.
            let scenes = client.scenes().list().await?;
            let names: Vec<String> = scenes.scenes.iter().map(|s| s.id.name.clone()).collect();
            let current_name = scenes
                .current_program_scene
                .map(|id| id.name)
                .unwrap_or_default();
            let index = names.iter().position(|n| n == &current_name).unwrap_or(0);
            info!("Active scene requested: {current_name} (index {index})");
            resp_tx
                .send(BridgeResponse::ActiveScene {
                    index,
                    name: current_name,
                })
                .await?;
        }

        BridgeCommand::SetSceneByName(name) => {
            info!("Switching to scene: {name}");
            client
                .scenes()
                .set_current_program_scene(name.as_str())
                .await?;
            // Scene change event will send the /activeSceneReturn automatically
            let _ = status_tx.send(AppStatus::Connected {
                scene: name.clone(),
            });
        }

        BridgeCommand::SetSceneByIndex(idx) => {
            let scenes = client.scenes().list().await?;
            let names: Vec<String> = scenes.scenes.iter().map(|s| s.id.name.clone()).collect();
            // OSC index is 1-based
            if idx < 1 {
                warn!("Invalid scene index {idx} (must be >= 1)");
                resp_tx.send(BridgeResponse::SceneList(names)).await?;
                return Ok(());
            }
            let zero_idx = (idx - 1) as usize;
            if let Some(name) = names.get(zero_idx) {
                info!("Switching to scene by index {idx}: {name}");
                client
                    .scenes()
                    .set_current_program_scene(name.as_str())
                    .await?;
                let _ = status_tx.send(AppStatus::Connected {
                    scene: name.clone(),
                });
            } else {
                warn!(
                    "Scene index {idx} out of range (have {} scenes)",
                    names.len()
                );
                // Send back the scene list so TouchOSC can resync
                resp_tx.send(BridgeResponse::SceneList(names)).await?;
            }
        }
    }

    Ok(())
}

/// Push the current scene list to TouchOSC and update the poll cache.
/// Called when scenes are created, removed, renamed, or reordered in OBS.
async fn push_scene_list(
    client: &obws::Client,
    resp_tx: &mpsc::Sender<BridgeResponse>,
    cached_scene_list: &mut Vec<String>,
) {
    match client.scenes().list().await {
        Ok(scenes) => {
            let names: Vec<String> = scenes.scenes.iter().map(|s| s.id.name.clone()).collect();
            info!("Pushing updated scene list: {} scenes", names.len());
            *cached_scene_list = names.clone();
            let _ = resp_tx.send(BridgeResponse::SceneList(names)).await;
        }
        Err(e) => {
            warn!("Failed to fetch scene list for push: {e}");
        }
    }
}

/// Check if an error is likely a connection/disconnect error.
fn is_connection_error(e: &anyhow::Error) -> bool {
    let msg = format!("{e:?}").to_lowercase();
    msg.contains("disconnect")
        || msg.contains("connection")
        || msg.contains("websocket")
        || msg.contains("closed")
        || msg.contains("broken pipe")
}
