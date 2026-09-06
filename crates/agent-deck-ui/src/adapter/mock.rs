use super::StreamAdapter;
use agent_deck_core::{AgentState, SessionEvent, SessionMetadata};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[allow(dead_code)]
pub struct MockAdapter {
    pub enabled: Arc<AtomicBool>,
}

#[allow(dead_code)]
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
                step_idx = (step_idx + 1) % 6;

                let events = match step_idx {
                    0 => vec![
                        // Windows Native Session 1
                        SessionEvent::new(
                            "win-gemini-1",
                            "Gemini • Terminal 1",
                            "Gemini",
                            AgentState::RunningTool {
                                name: "replace_file_content".to_string(),
                                summary: "Patching auth middleware token TTL".to_string(),
                            },
                            "EDIT: Patching JWT token TTL in session.rs (line 42)",
                            counter,
                            SessionMetadata {
                                host: "Windows".to_string(),
                                tmux_session: None,
                                tmux_window: None,
                                tmux_pane: None,
                                cwd: Some("C:\\Users\\schordinger\\workbench\\agent-deck".to_string()),
                                agent_type: None,
                                pid: Some(12480),
                            },
                        ),
                        // Windows Native Session 2
                        SessionEvent::new(
                            "win-gemini-2",
                            "Gemini • Terminal 2",
                            "Gemini",
                            AgentState::Idle,
                            "IDLE: Listening for next instruction in PowerShell",
                            counter + 5,
                            SessionMetadata {
                                host: "Windows".to_string(),
                                tmux_session: None,
                                tmux_window: None,
                                tmux_pane: None,
                                cwd: Some("C:\\Users\\schordinger\\workbench".to_string()),
                                agent_type: None,
                                pid: Some(14200),
                            },
                        ),
                        // WSL2 Session 1 (tmux backend)
                        SessionEvent::new(
                            "wsl-gemini-backend",
                            "Gemini • tmux:backend:0.1",
                            "Gemini",
                            AgentState::WaitingForInput {
                                prompt_preview: "Proceed with running database migrations on postgres-dev? [Y/n]".to_string(),
                            },
                            "INPUT REQUIRED: Confirm running migrations on postgres-dev [Y/n]",
                            counter + 10,
                            SessionMetadata {
                                host: "WSL2-Ubuntu".to_string(),
                                tmux_session: Some("backend".to_string()),
                                tmux_window: Some("0:api-server".to_string()),
                                tmux_pane: Some("%1".to_string()),
                                cwd: Some("/home/schordinger/workbench/api".to_string()),
                                agent_type: None,
                                pid: Some(4092),
                            },
                        ),
                        // WSL2 Session 2 (tmux worker)
                        SessionEvent::new(
                            "wsl-gemini-worker",
                            "Gemini • tmux:worker:0.0",
                            "Gemini",
                            AgentState::RunningTool {
                                name: "cargo_test".to_string(),
                                summary: "Running integration test suite".to_string(),
                            },
                            "TEST: cargo test --test api_gateway (18/22 passed)",
                            counter + 24,
                            SessionMetadata {
                                host: "WSL2-Ubuntu".to_string(),
                                tmux_session: Some("worker".to_string()),
                                tmux_window: Some("0:tests".to_string()),
                                tmux_pane: Some("%0".to_string()),
                                cwd: Some("/home/schordinger/workbench/worker".to_string()),
                                agent_type: None,
                                pid: Some(5120),
                            },
                        ),
                        // WSL2 Session 3 (direct terminal)
                        SessionEvent::new(
                            "wsl-gemini-frontend",
                            "Gemini • Shell",
                            "Gemini",
                            AgentState::Thinking,
                            "THINKING: Analyzing call hierarchy for router.rs",
                            counter + 2,
                            SessionMetadata {
                                host: "WSL2-Ubuntu".to_string(),
                                tmux_session: None,
                                tmux_window: None,
                                tmux_pane: None,
                                cwd: Some("/home/schordinger/workbench/frontend".to_string()),
                                agent_type: None,
                                pid: Some(6780),
                            },
                        ),
                    ],
                    1 => vec![
                        // Windows Native Session 1 -> Thinking
                        SessionEvent::new(
                            "win-gemini-1",
                            "Gemini • Terminal 1",
                            "Gemini",
                            AgentState::Thinking,
                            "THINKING: Verifying Rust borrow checker rules for token lifetime",
                            counter,
                            SessionMetadata {
                                host: "Windows".to_string(),
                                tmux_session: None,
                                tmux_window: None,
                                tmux_pane: None,
                                cwd: Some("C:\\Users\\schordinger\\workbench\\agent-deck".to_string()),
                                agent_type: None,
                                pid: Some(12480),
                            },
                        ),
                        // Windows Native Session 2 -> Waiting for input
                        SessionEvent::new(
                            "win-gemini-2",
                            "Gemini • Terminal 2",
                            "Gemini",
                            AgentState::WaitingForInput {
                                prompt_preview: "Allow execution of cargo build --release? [y/N]".to_string(),
                            },
                            "PERMISSION REQUIRED: Approve running cargo build --release [y/N]",
                            counter + 5,
                            SessionMetadata {
                                host: "Windows".to_string(),
                                tmux_session: None,
                                tmux_window: None,
                                tmux_pane: None,
                                cwd: Some("C:\\Users\\schordinger\\workbench".to_string()),
                                agent_type: None,
                                pid: Some(14200),
                            },
                        ),
                        // WSL2 Session 1 -> Thinking
                        SessionEvent::new(
                            "wsl-gemini-backend",
                            "Gemini • tmux:backend:0.1",
                            "Gemini",
                            AgentState::Thinking,
                            "THINKING: Evaluating diesel ORM schema changes against user models",
                            counter + 10,
                            SessionMetadata {
                                host: "WSL2-Ubuntu".to_string(),
                                tmux_session: Some("backend".to_string()),
                                tmux_window: Some("0:api-server".to_string()),
                                tmux_pane: Some("%1".to_string()),
                                cwd: Some("/home/schordinger/workbench/api".to_string()),
                                agent_type: None,
                                pid: Some(4092),
                            },
                        ),
                        // WSL2 Session 2 -> Waiting for input
                        SessionEvent::new(
                            "wsl-gemini-worker",
                            "Gemini • tmux:worker:0.0",
                            "Gemini",
                            AgentState::WaitingForInput {
                                prompt_preview: "Allow execution of bash deploy script? [y/N]".to_string(),
                            },
                            "PERMISSION REQUIRED: Execute bash script scripts/deploy.sh [y/N]",
                            counter + 24,
                            SessionMetadata {
                                host: "WSL2-Ubuntu".to_string(),
                                tmux_session: Some("worker".to_string()),
                                tmux_window: Some("0:tests".to_string()),
                                tmux_pane: Some("%0".to_string()),
                                cwd: Some("/home/schordinger/workbench/worker".to_string()),
                                agent_type: None,
                                pid: Some(5120),
                            },
                        ),
                    ],
                    2 => vec![
                        // Windows Native Session 1 -> Running tool
                        SessionEvent::new(
                            "win-gemini-1",
                            "Gemini • Terminal 1",
                            "Gemini",
                            AgentState::RunningTool {
                                name: "cargo_check".to_string(),
                                summary: "Checking workspace crates".to_string(),
                            },
                            "CARGO: cargo check --workspace (Finished in 1.4s)",
                            counter,
                            SessionMetadata {
                                host: "Windows".to_string(),
                                tmux_session: None,
                                tmux_window: None,
                                tmux_pane: None,
                                cwd: Some("C:\\Users\\schordinger\\workbench\\agent-deck".to_string()),
                                agent_type: None,
                                pid: Some(12480),
                            },
                        ),
                        // WSL2 Session 1 -> Running tool
                        SessionEvent::new(
                            "wsl-gemini-backend",
                            "Gemini • tmux:backend:0.1",
                            "Gemini",
                            AgentState::RunningTool {
                                name: "run_command".to_string(),
                                summary: "Executing diesel migration run".to_string(),
                            },
                            "RUNNING: diesel migration run --database-url=postgres://localhost:5432/db",
                            counter + 10,
                            SessionMetadata {
                                host: "WSL2-Ubuntu".to_string(),
                                tmux_session: Some("backend".to_string()),
                                tmux_window: Some("0:api-server".to_string()),
                                tmux_pane: Some("%1".to_string()),
                                cwd: Some("/home/schordinger/workbench/api".to_string()),
                                agent_type: None,
                                pid: Some(4092),
                            },
                        ),
                    ],
                    3 => vec![
                        // Windows Native Session 1 -> Finished
                        SessionEvent::new(
                            "win-gemini-1",
                            "Gemini • Terminal 1",
                            "Gemini",
                            AgentState::Finished,
                            "ALL TASKS COMPLETED: Auth middleware refactored & checked",
                            counter,
                            SessionMetadata {
                                host: "Windows".to_string(),
                                tmux_session: None,
                                tmux_window: None,
                                tmux_pane: None,
                                cwd: Some("C:\\Users\\schordinger\\workbench\\agent-deck".to_string()),
                                agent_type: None,
                                pid: Some(12480),
                            },
                        ),
                        // WSL2 Session 1 -> Finished
                        SessionEvent::new(
                            "wsl-gemini-backend",
                            "Gemini • tmux:backend:0.1",
                            "Gemini",
                            AgentState::Finished,
                            "ALL MIGRATIONS APPLIED: Schema version up to date (004_add_users)",
                            counter + 10,
                            SessionMetadata {
                                host: "WSL2-Ubuntu".to_string(),
                                tmux_session: Some("backend".to_string()),
                                tmux_window: Some("0:api-server".to_string()),
                                tmux_pane: Some("%1".to_string()),
                                cwd: Some("/home/schordinger/workbench/api".to_string()),
                                agent_type: None,
                                pid: Some(4092),
                            },
                        ),
                        // WSL2 Session 2 -> Finished
                        SessionEvent::new(
                            "wsl-gemini-worker",
                            "Gemini • tmux:worker:0.0",
                            "Gemini",
                            AgentState::Finished,
                            "ALL TESTS PASSED: 22/22 integration tests green",
                            counter + 24,
                            SessionMetadata {
                                host: "WSL2-Ubuntu".to_string(),
                                tmux_session: Some("worker".to_string()),
                                tmux_window: Some("0:tests".to_string()),
                                tmux_pane: Some("%0".to_string()),
                                cwd: Some("/home/schordinger/workbench/worker".to_string()),
                                agent_type: None,
                                pid: Some(5120),
                            },
                        ),
                    ],
                    _ => vec![
                        // Windows Native Session 1 -> Idle
                        SessionEvent::new(
                            "win-gemini-1",
                            "Gemini • Terminal 1",
                            "Gemini",
                            AgentState::Idle,
                            "IDLE: Ready for next task in Windows terminal",
                            counter,
                            SessionMetadata {
                                host: "Windows".to_string(),
                                tmux_session: None,
                                tmux_window: None,
                                tmux_pane: None,
                                cwd: Some("C:\\Users\\schordinger\\workbench\\agent-deck".to_string()),
                                agent_type: None,
                                pid: Some(12480),
                            },
                        ),
                        // WSL2 Session 1 -> Idle
                        SessionEvent::new(
                            "wsl-gemini-backend",
                            "Gemini • tmux:backend:0.1",
                            "Gemini",
                            AgentState::Idle,
                            "IDLE: Session active in tmux:backend. Listening for prompt",
                            counter + 10,
                            SessionMetadata {
                                host: "WSL2-Ubuntu".to_string(),
                                tmux_session: Some("backend".to_string()),
                                tmux_window: Some("0:api-server".to_string()),
                                tmux_pane: Some("%1".to_string()),
                                cwd: Some("/home/schordinger/workbench/api".to_string()),
                                agent_type: None,
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
