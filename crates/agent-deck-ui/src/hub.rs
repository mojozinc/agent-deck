use agent_deck_core::{AgentState, SessionEvent, SessionMetadata};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct ActiveSession {
    pub session_id: String,
    pub display_name: String,
    pub agent_type: String,
    pub state: AgentState,
    pub status_text: String,
    pub step_count: u32,
    pub metadata: SessionMetadata,
    pub last_updated: Instant,
    pub marquee_offset: f32,
    pub vu_levels: [f32; 8],
}

pub struct SessionHub {
    pub sessions: Vec<ActiveSession>,
    pub rx: Receiver<SessionEvent>,
    pub tx: Sender<SessionEvent>,
    pub selected_tab_idx: usize, // 0 = Gemini (Windows), 1 = Gemini (WSL2)
}

impl SessionHub {
    pub fn new() -> Self {
        let (tx, rx) = channel::<SessionEvent>();

        // Default initial session placeholders
        let initial_windows = ActiveSession {
            session_id: "win-gemini-1".to_string(),
            display_name: "Gemini Win Session #1".to_string(),
            agent_type: "Gemini".to_string(),
            state: AgentState::Idle,
            status_text: "SCANNING NATIVE ANTIGRAVITY BRAIN...".to_string(),
            step_count: 0,
            metadata: SessionMetadata {
                host: "Windows".to_string(),
                tmux_session: None,
                tmux_window: None,
                tmux_pane: None,
                cwd: None,
                pid: None,
            },
            last_updated: Instant::now(),
            marquee_offset: 0.0,
            vu_levels: [0.0; 8],
        };

        Self {
            sessions: vec![initial_windows],
            rx,
            tx,
            selected_tab_idx: 0,
        }
    }

    pub fn sender(&self) -> Sender<SessionEvent> {
        self.tx.clone()
    }

    /// Ingests all pending events from active stream adapters
    pub fn poll_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            if let Some(existing) = self.sessions.iter_mut().find(|s| s.session_id == event.session_id) {
                existing.display_name = event.display_name;
                existing.agent_type = event.agent_type;
                existing.state = event.state;
                existing.status_text = event.status_text;
                existing.step_count = event.step_count;
                existing.metadata = event.metadata;
                existing.last_updated = Instant::now();
            } else {
                self.sessions.push(ActiveSession {
                    session_id: event.session_id,
                    display_name: event.display_name,
                    agent_type: event.agent_type,
                    state: event.state,
                    status_text: event.status_text,
                    step_count: event.step_count,
                    metadata: event.metadata,
                    last_updated: Instant::now(),
                    marquee_offset: 0.0,
                    vu_levels: [0.0; 8],
                });
            }
        }
    }

    /// Returns sessions running natively on Windows
    pub fn windows_sessions(&self) -> Vec<&ActiveSession> {
        self.sessions
            .iter()
            .filter(|s| s.metadata.host.eq_ignore_ascii_case("windows") || s.session_id.starts_with("win-"))
            .collect()
    }

    /// Returns sessions running inside WSL2
    pub fn wsl2_sessions(&self) -> Vec<&ActiveSession> {
        self.sessions
            .iter()
            .filter(|s| !s.metadata.host.eq_ignore_ascii_case("windows") && !s.session_id.starts_with("win-"))
            .collect()
    }

    /// Checks if any session in the given list requires user input
    pub fn has_waiting_input(sessions: &[&ActiveSession]) -> bool {
        sessions.iter().any(|s| matches!(s.state, AgentState::WaitingForInput { .. }))
    }
}
