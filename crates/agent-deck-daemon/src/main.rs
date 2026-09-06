mod tmux;
mod transcript;

use agent_deck_core::{AgentState, SessionEvent, SessionMetadata};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, Mutex};
use transcript::TranscriptWatcher;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let distro = std::env::var("WSL_DISTRO_NAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "clibox".to_string());

    println!("🎛️ Agent Deck WSL2 Bridge Daemon v0.1 starting for [{}]...", distro);
    let bind_addr: SocketAddr = "0.0.0.0:8765".parse()?;
    let listener = TcpListener::bind(bind_addr).await?;
    println!("📡 Listening for Windows Deck UI connections on {}", bind_addr);

    let (tx, _rx) = broadcast::channel::<String>(100);
    let tx_broadcast = tx.clone();
    let watcher_mutex = Arc::new(Mutex::new(TranscriptWatcher::new()));

    // Perform initial scan on startup so current state is immediately warm
    {
        let mut watcher = watcher_mutex.lock().await;
        let initial_events = watcher.scan_and_collect_events();
        println!("🔍 Initial scan found {} active sessions in [{}]", initial_events.len(), distro);
    }

    let watcher_bg = watcher_mutex.clone();
    let distro_bg = distro.clone();

    // 1. Background Scanner Thread (polling transcripts & tmux)
    tokio::spawn(async move {
        let mut heartbeat_tick: u32 = 0;
        loop {
            tokio::time::sleep(Duration::from_millis(400)).await;
            heartbeat_tick += 1;

            let mut watcher = watcher_bg.lock().await;
            let events = watcher.scan_and_collect_events();
            for event in events {
                if let Ok(json) = serde_json::to_string(&event) {
                    let _ = tx_broadcast.send(json);
                }
            }

            // Periodic Bridge Heartbeat (every ~3 seconds)
            if heartbeat_tick % 8 == 0 {
                let hb_event = SessionEvent::new(
                    format!("wsl-bridge-{}", distro_bg),
                    format!("WSL Bridge [{}]", distro_bg),
                    "Bridge",
                    AgentState::Idle,
                    format!("WSL2 bridge online ({})", distro_bg),
                    0,
                    SessionMetadata {
                        host: format!("wsl:{}", distro_bg),
                        tmux_session: None,
                        tmux_window: None,
                        tmux_pane: None,
                        cwd: None,
                        pid: None,
                        agent_type: Some("Bridge".to_string()),
                    },
                );
                if let Ok(json) = serde_json::to_string(&hb_event) {
                    let _ = tx_broadcast.send(json);
                }
            }
        }
    });

    // 2. TCP Client Connection Loop
    loop {
        let (socket, client_addr) = listener.accept().await?;
        println!("🔌 Windows Agent Deck connected from: {}", client_addr);
        let mut rx = tx.subscribe();
        let (_reader, mut writer) = socket.into_split();
        let distro_conn = distro.clone();
        let watcher_conn = watcher_mutex.clone();

        tokio::spawn(async move {
            // Send immediate handshake on connect
            let handshake = SessionEvent::new(
                format!("wsl-bridge-{}", distro_conn),
                format!("WSL Bridge [{}]", distro_conn),
                "Bridge",
                AgentState::Idle,
                format!("Connected to {}", distro_conn),
                0,
                SessionMetadata {
                    host: format!("wsl:{}", distro_conn),
                    tmux_session: None,
                    tmux_window: None,
                    tmux_pane: None,
                    cwd: None,
                    pid: None,
                    agent_type: Some("Bridge".to_string()),
                },
            );

            if let Ok(json) = serde_json::to_string(&handshake) {
                let payload = format!("{}\n", json);
                let _ = writer.write_all(payload.as_bytes()).await;
            }

            // Immediately send ALL currently known active sessions to the newly connected UI!
            let current_sessions = {
                let watcher = watcher_conn.lock().await;
                watcher.get_latest_sessions()
            };

            for event in current_sessions {
                if let Ok(json) = serde_json::to_string(&event) {
                    let payload = format!("{}\n", json);
                    let _ = writer.write_all(payload.as_bytes()).await;
                }
            }

            // Forward live broadcast stream, handling lagged subscriber error without terminating TCP client stream
            loop {
                match rx.recv().await {
                    Ok(msg) => {
                        let payload = format!("{}\n", msg);
                        if let Err(e) = writer.write_all(payload.as_bytes()).await {
                            eprintln!("Client disconnected: {}", e);
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        eprintln!("Broadcast receiver lagged by {} messages; continuing live stream", skipped);
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }
        });
    }
}
