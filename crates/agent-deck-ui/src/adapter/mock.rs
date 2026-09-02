use super::StreamAdapter;
use agent_deck_core::{AgentState, SessionEvent, SessionMetadata};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub struct MockAdapter {
    pub enabled: Arc<AtomicBool>,
}

impl MockAdapter {
    pub fn new(enabled: Arc<AtomicBool>) -> Self {
        Self { enabled }
    }
}

impl StreamAdapter for MockAdapter {
    fn name(&self) -> &'static str {
        "Mock Simulation Adapter"
    }

    fn start(&mut self, tx: Sender<SessionEvent>) {
        let enabled = self.enabled.clone();

        thread::spawn(move || {
            let mut step_idx = 0;
            let mut counter: u32 = 40;

            loop {
                thread::sleep(Duration::from_millis(3200));

                if !enabled.load(Ordering::Relaxed) {
                    continue;
                }

                counter += 1;
                step_idx = (step_idx + 1) % 5;

                let events = match step_idx {
                    0 => vec![
                        SessionEvent::new(
                            "mock-wsl-tmux-backend",
                            "AGY [tmux:backend:0.1]",
                            "AGY-WSL",
                            AgentState::WaitingForInput {
                                prompt_preview: "Proceed with running database migrations? [Y/n]".to_string(),
                            },
                            "INPUT REQUIRED: Confirm running migrations on postgres-dev [Y/n]",
                            counter,
                            SessionMetadata {
                                host: "WSL2-Ubuntu".to_string(),
                                tmux_session: Some("backend".to_string()),
                                tmux_window: Some("0:api-server".to_string()),
                                tmux_pane: Some("%1".to_string()),
                                cwd: Some("/home/schordinger/workbench/api".to_string()),
                                pid: Some(4092),
                            },
                        ),
                        SessionEvent::new(
                            "mock-wsl-worker",
                            "AGY (WSL2 Worker)",
                            "AGY-WSL",
                            AgentState::RunningTool {
                                name: "cargo_test".to_string(),
                                summary: "Running integration test suite".to_string(),
                            },
                            "TEST: cargo test --test api_gateway [18/22 passed]",
                            counter + 15,
                            SessionMetadata {
                                host: "WSL2-Ubuntu".to_string(),
                                tmux_session: None,
                                tmux_window: None,
                                tmux_pane: None,
                                cwd: None,
                                pid: None,
                            },
                        ),
                    ],
                    1 => vec![
                        SessionEvent::new(
                            "mock-wsl-tmux-backend",
                            "AGY [tmux:backend:0.1]",
                            "AGY-WSL",
                            AgentState::Thinking,
                            "THINKING: Evaluating schema migration against diesel ORM models",
                            counter,
                            SessionMetadata {
                                host: "WSL2-Ubuntu".to_string(),
                                tmux_session: Some("backend".to_string()),
                                tmux_window: Some("0:api-server".to_string()),
                                tmux_pane: Some("%1".to_string()),
                                cwd: Some("/home/schordinger/workbench/api".to_string()),
                                pid: Some(4092),
                            },
                        ),
                        SessionEvent::new(
                            "mock-claude-wsl",
                            "Claude (WSL2)",
                            "CLAUDE-WSL",
                            AgentState::WaitingForInput {
                                prompt_preview: "Confirm file overwrite in auth/jwt.rs [y/N]".to_string(),
                            },
                            "PERMISSION REQUIRED: Approve file overwrite for auth/jwt.rs [y/N]",
                            counter + 8,
                            SessionMetadata {
                                host: "WSL2-Ubuntu".to_string(),
                                tmux_session: Some("frontend".to_string()),
                                tmux_window: Some("1:web".to_string()),
                                tmux_pane: Some("%2".to_string()),
                                cwd: None,
                                pid: None,
                            },
                        ),
                    ],
                    2 => vec![
                        SessionEvent::new(
                            "mock-wsl-tmux-backend",
                            "AGY [tmux:backend:0.1]",
                            "AGY-WSL",
                            AgentState::RunningTool {
                                name: "run_command".to_string(),
                                summary: "Executing diesel migration run".to_string(),
                            },
                            "RUNNING: diesel migration run --database-url=postgres://localhost:5432/db",
                            counter,
                            SessionMetadata {
                                host: "WSL2-Ubuntu".to_string(),
                                tmux_session: Some("backend".to_string()),
                                tmux_window: Some("0:api-server".to_string()),
                                tmux_pane: Some("%1".to_string()),
                                cwd: Some("/home/schordinger/workbench/api".to_string()),
                                pid: Some(4092),
                            },
                        ),
                    ],
                    3 => vec![
                        SessionEvent::new(
                            "mock-wsl-tmux-backend",
                            "AGY [tmux:backend:0.1]",
                            "AGY-WSL",
                            AgentState::Finished,
                            "ALL MIGRATIONS APPLIED: Schema version is up to date (004_add_users)",
                            counter,
                            SessionMetadata {
                                host: "WSL2-Ubuntu".to_string(),
                                tmux_session: Some("backend".to_string()),
                                tmux_window: Some("0:api-server".to_string()),
                                tmux_pane: Some("%1".to_string()),
                                cwd: Some("/home/schordinger/workbench/api".to_string()),
                                pid: Some(4092),
                            },
                        ),
                    ],
                    _ => vec![
                        SessionEvent::new(
                            "mock-wsl-tmux-backend",
                            "AGY [tmux:backend:0.1]",
                            "AGY-WSL",
                            AgentState::Idle,
                            "AGY IDLE: Session active in tmux:backend. Listening for next prompt...",
                            counter,
                            SessionMetadata {
                                host: "WSL2-Ubuntu".to_string(),
                                tmux_session: Some("backend".to_string()),
                                tmux_window: Some("0:api-server".to_string()),
                                tmux_pane: Some("%1".to_string()),
                                cwd: Some("/home/schordinger/workbench/api".to_string()),
                                pid: Some(4092),
                            },
                        ),
                    ],
                };

                for ev in events {
                    let _ = tx.send(ev);
                }
            }
        });
    }
}

