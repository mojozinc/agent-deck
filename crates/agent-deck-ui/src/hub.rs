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
}

pub struct SessionHub {
    pub sessions: Vec<ActiveSession>,
    pub rx: Receiver<SessionEvent>,
    pub tx: Sender<SessionEvent>,
}

impl SessionHub {
    pub fn new() -> Self {
        let (tx, rx) = channel::<SessionEvent>();

        // Default initial session placeholder
        let initial_session = ActiveSession {
            session_id: "win-agy-default".to_string(),
            display_name: "AGY (Win Native)".to_string(),
            agent_type: "AGY-WIN".to_string(),
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
        };

        Self {
            sessions: vec![initial_session],
            rx,
            tx,
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
                });
            }
        }
    }
}

