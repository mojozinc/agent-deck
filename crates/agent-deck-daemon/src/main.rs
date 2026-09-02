mod tmux;
mod transcript;

use agent_deck_core::SessionEvent;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, Mutex};
use transcript::TranscriptWatcher;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🎛️ Agent Deck WSL2 Bridge Daemon v0.1 starting...");
    let bind_addr: SocketAddr = "0.0.0.0:8765".parse()?;
    let listener = TcpListener::bind(bind_addr).await?;
    println!("📡 Listening for Windows Deck UI connections on {}", bind_addr);

    let (tx, _rx) = broadcast::channel::<String>(100);
    let tx_broadcast = tx.clone();

    // 1. Background Scanner Thread (polling transcripts & tmux)
    tokio::spawn(async move {
        let mut watcher = TranscriptWatcher::new();
        loop {
            tokio::time::sleep(Duration::from_millis(400)).await;
            let events = watcher.scan_and_collect_events();
            for event in events {
                if let Ok(json) = serde_json::to_string(&event) {
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

        tokio::spawn(async move {
            let (mut reader, mut writer) = socket.into_split();
            while let Ok(msg) = rx.recv().await {
                let payload = format!("{}\n", msg);
                if let Err(e) = writer.write_all(payload.as_bytes()).await {
                    eprintln!("Client disconnected: {}", e);
                    break;
                }
            }
        });
    }
}

