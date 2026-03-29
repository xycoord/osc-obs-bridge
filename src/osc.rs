use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceInfo};
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
    let resolved_listen_host = config.resolved_osc_listen_host();
    let listen_addr = format!("{resolved_listen_host}:{}", config.osc_listen_port);
    let socket = UdpSocket::bind(&listen_addr)
        .await
        .with_context(|| format!("Failed to bind OSC socket on {listen_addr}"))?;

    // Enable broadcast so we can send to x.x.x.255 addresses
    socket.set_broadcast(true)?;

    info!("OSC listening on {listen_addr}");
    // Only set OscListening if no error status has already been set by the OBS task
    status_tx.send_if_modified(|current| {
        if matches!(current, AppStatus::Starting) {
            *current = AppStatus::OscListening;
            true
        } else {
            false
        }
    });

    let socket = Arc::new(socket);
    let send_port = config.osc_send_port;
    let resolved_send_host = config.resolved_osc_send_host(&resolved_listen_host);
    let default_send_addr: SocketAddr = format!("{resolved_send_host}:{send_port}")
        .parse()
        .with_context(|| format!("Invalid OSC send address: {resolved_send_host}:{send_port}"))?;

    info!("OSC sending to {default_send_addr}");

    // Advertise the OSC service via mDNS/Zeroconf so TouchOSC can discover it
    let _mdns = register_mdns_service(&resolved_listen_host, config.osc_listen_port);

    // Track the last client IP we received a message from.
    // This lets us reply to the tablet even if its IP differs from the configured send address.
    // We store only the IP — responses are always sent to the configured osc_send_port.
    let last_client_ip: Arc<Mutex<Option<std::net::IpAddr>>> = Arc::new(Mutex::new(None));

    // Spawn the outbound sender task
    let send_socket = Arc::clone(&socket);
    let send_client_ip = Arc::clone(&last_client_ip);
    let mut sender_handle = tokio::spawn(async move {
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

            // Also send to the last-known client IP (on the configured port)
            // if it's different from the default address
            let client_ip = *send_client_ip.lock().await;
            if let Some(ip) = client_ip {
                let client_addr = SocketAddr::new(ip, send_port);
                if client_addr != default_send_addr {
                    if let Err(e) = send_socket.send_to(&bytes, client_addr).await {
                        debug!("Failed to send OSC to client {client_addr}: {e}");
                    }
                }
            }
        }
    });

    // Inbound listener loop
    let mut buf = [0u8; 4096];
    loop {
        tokio::select! {
            result = socket.recv_from(&mut buf) => {
                let (len, src_addr) = match result {
                    Ok(r) => r,
                    Err(e) => {
                        error!("OSC recv error: {e}");
                        continue;
                    }
                };

                // Remember the client's IP (not port — responses use the configured send port)
                *last_client_ip.lock().await = Some(src_addr.ip());

                let packet = match rosc::decoder::decode_udp(&buf[..len]) {
                    Ok((_rest, packet)) => packet,
                    Err(e) => {
                        warn!("Failed to decode OSC from {src_addr}: {e}");
                        continue;
                    }
                };

                handle_packet(&packet, &cmd_tx).await;
            }
            _ = &mut sender_handle => {
                // Sender task exited (resp_rx channel closed), we should exit too
                info!("OSC sender task ended");
                break;
            }
        }
    }

    Ok(())
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

/// Register an mDNS service so TouchOSC can discover this bridge via its "Browse" button.
/// Returns the ServiceDaemon (must be kept alive for the advertisement to persist).
fn register_mdns_service(host: &str, port: u16) -> Option<ServiceDaemon> {
    let mdns = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            warn!("Failed to start mDNS daemon: {e}");
            return None;
        }
    };

    let service_type = "_osc._udp.local.";
    let instance_name = "osc-obs-bridge";
    let host_fqdn = format!("{host}.local.");

    let properties: &[(&str, &str)] = &[];
    let service_info = match ServiceInfo::new(
        service_type,
        instance_name,
        &host_fqdn,
        host,
        port,
        properties,
    ) {
        Ok(info) => info,
        Err(e) => {
            warn!("Failed to create mDNS service info: {e}");
            return None;
        }
    };

    match mdns.register(service_info) {
        Ok(_) => {
            info!("mDNS: advertising as '{instance_name}' on {host}:{port} (_osc._udp)");
        }
        Err(e) => {
            warn!("Failed to register mDNS service: {e}");
            return None;
        }
    }

    Some(mdns)
}
