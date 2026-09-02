use super::StreamAdapter;
use agent_deck_core::SessionEvent;
use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

pub struct Wsl2BridgeAdapter {
    target_addr: String,
}

impl Wsl2BridgeAdapter {
    pub fn new(target_addr: impl Into<String>) -> Self {
        Self {
            target_addr: target_addr.into(),
        }
    }
}

impl StreamAdapter for Wsl2BridgeAdapter {
    fn name(&self) -> &'static str {
        "WSL2 Activity Bridge Adapter"
    }

    fn start(&mut self, tx: Sender<SessionEvent>) {
        let addr = self.target_addr.clone();

        thread::spawn(move || loop {
            match TcpStream::connect(&addr) {
                Ok(stream) => {
                    let reader = BufReader::new(stream);
                    for line in reader.lines().flatten() {
                        if !line.trim().is_empty() {
                            if let Ok(event) = serde_json::from_str::<SessionEvent>(&line) {
                                let _ = tx.send(event);
                            }
                        }
                    }
                }
                Err(_) => {
                    // Daemon not running or WSL sleeping; wait and retry
                    thread::sleep(Duration::from_secs(2));
                }
            }
        });
    }
}

