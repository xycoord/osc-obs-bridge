use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use rosc::{OscMessage, OscPacket, OscType};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch, Mutex};
use tracing::{debug, error, info, warn};

use crate::bridge::{AppStatus, BridgeCommand, BridgeResponse};
use crate::config::Config;

/// Run the OSC listener (inbound) and sender (outbound) tasks.
///
/// - Listens for OSC messages on UDP and translates them to `BridgeCommand`s.
/// - Receives `BridgeResponse`s and sends them as OSC messages back to the client.
pub async fn run(
    config: Config,
    cmd_tx: mpsc::Sender<BridgeCommand>,
    mut resp_rx: mpsc::Receiver<BridgeResponse>,
    status_tx: watch::Sender<AppStatus>,
) -> Result<()> {
    let listen_addr = format!("{}:{}", config.osc_listen_host, config.osc_listen_port);
    let socket = UdpSocket::bind(&listen_addr)
        .await
        .with_context(|| format!("Failed to bind OSC socket on {listen_addr}"))?;

    info!("OSC listening on {listen_addr}");
    let _ = status_tx.send(AppStatus::OscListening);

    let socket = Arc::new(socket);
    let default_send_addr: SocketAddr = format!("{}:{}", config.osc_send_host, config.osc_send_port)
        .parse()
        .context("Invalid OSC send address")?;

    // Track the last client address we received a message from.
    // This lets us reply to the tablet even if its IP differs from the configured send address.
    let last_client_addr: Arc<Mutex<Option<SocketAddr>>> = Arc::new(Mutex::new(None));

    // Spawn the outbound sender task
    let send_socket = Arc::clone(&socket);
    let send_client_addr = Arc::clone(&last_client_addr);
    tokio::spawn(async move {
        while let Some(response) = resp_rx.recv().await {
            let packet = response_to_osc(&response);
            let bytes = match rosc::encoder::encode(&packet) {
                Ok(b) => b,
                Err(e) => {
                    error!("Failed to encode OSC response: {e}");
                    continue;
                }
            };

            // Send to the configured default address
            if let Err(e) = send_socket.send_to(&bytes, default_send_addr).await {
                error!("Failed to send OSC to {default_send_addr}: {e}");
            }

            // Also send to the last-known client if it's different from the default
            let client = send_client_addr.lock().await;
            if let Some(addr) = *client {
                if addr != default_send_addr {
                    if let Err(e) = send_socket.send_to(&bytes, addr).await {
                        debug!("Failed to send OSC to client {addr}: {e}");
                    }
                }
            }
        }
    });

    // Inbound listener loop
    let mut buf = [0u8; 4096];
    loop {
        let (len, src_addr) = match socket.recv_from(&mut buf).await {
            Ok(r) => r,
            Err(e) => {
                error!("OSC recv error: {e}");
                continue;
            }
        };

        // Remember who sent us a message
        *last_client_addr.lock().await = Some(src_addr);

        let packet = match rosc::decoder::decode_udp(&buf[..len]) {
            Ok((_rest, packet)) => packet,
            Err(e) => {
                warn!("Failed to decode OSC from {src_addr}: {e}");
                continue;
            }
        };

        handle_packet(&packet, &cmd_tx).await;
    }
}

/// Recursively handle an OSC packet (message or bundle).
async fn handle_packet(packet: &OscPacket, cmd_tx: &mpsc::Sender<BridgeCommand>) {
    match packet {
        OscPacket::Message(msg) => {
            handle_message(msg, cmd_tx).await;
        }
        OscPacket::Bundle(bundle) => {
            for p in &bundle.content {
                // Use Box::pin to handle recursion in async
                Box::pin(handle_packet(p, cmd_tx)).await;
            }
        }
    }
}

/// Translate a single OSC message into a BridgeCommand.
async fn handle_message(msg: &OscMessage, cmd_tx: &mpsc::Sender<BridgeCommand>) {
    let addr = msg.addr.as_str();
    debug!("OSC recv: {} {:?}", addr, msg.args);

    let cmd = match addr {
        "/sceneList" => Some(BridgeCommand::GetSceneList),
        "/activeScene" => Some(BridgeCommand::GetActiveScene),
        "/scene" => parse_scene_command(&msg.args),
        _ => {
            debug!("Ignoring unknown OSC address: {addr}");
            None
        }
    };

    if let Some(cmd) = cmd {
        if let Err(e) = cmd_tx.send(cmd).await {
            error!("Failed to send command to OBS task: {e}");
        }
    }
}

/// Parse the arguments for a /scene command.
/// Accepts either a string (scene name) or an integer (1-based index).
fn parse_scene_command(args: &[OscType]) -> Option<BridgeCommand> {
    match args.first() {
        Some(OscType::String(name)) => Some(BridgeCommand::SetSceneByName(name.clone())),
        Some(OscType::Int(n)) => Some(BridgeCommand::SetSceneByIndex(*n)),
        Some(OscType::Float(f)) => Some(BridgeCommand::SetSceneByIndex(*f as i32)),
        _ => {
            warn!("/scene received with no valid argument: {args:?}");
            None
        }
    }
}

/// Convert a BridgeResponse into an OSC packet for sending.
fn response_to_osc(response: &BridgeResponse) -> OscPacket {
    match response {
        BridgeResponse::SceneList(scenes) => {
            let args: Vec<OscType> = scenes.iter().map(|s| OscType::String(s.clone())).collect();
            OscPacket::Message(OscMessage {
                addr: "/sceneListReturn".to_string(),
                args,
            })
        }
        BridgeResponse::ActiveScene { index, name } => OscPacket::Message(OscMessage {
            addr: "/activeSceneReturn".to_string(),
            args: vec![
                OscType::Int(*index as i32),
                OscType::String(name.clone()),
            ],
        }),
    }
}
