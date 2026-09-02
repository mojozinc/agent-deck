use agent_deck_core::{AgentState, SessionEvent, SessionMetadata};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Instant;

#[derive(Clone, Debug, PartialEq)]
pub struct AttentionState {
    pub is_acknowledged: bool,
    pub last_state_signature: String,
}

impl AttentionState {
    pub fn new() -> Self {
        Self {
            is_acknowledged: false,
            last_state_signature: String::new(),
        }
    }

    /// Updates the attention tracker with a new state. If state changed, resets acknowledgment!
    pub fn update(&mut self, state: &AgentState, step_count: u32) {
        let sig = match state {
            AgentState::WaitingForInput { prompt_preview } => format!("waiting:{}:{}", step_count, prompt_preview),
            AgentState::RunningTool { name, .. } => format!("tool:{}:{}", step_count, name),
            AgentState::Thinking => format!("thinking:{}", step_count),
            AgentState::Error { message } => format!("error:{}:{}", step_count, message),
            AgentState::Finished => format!("finished:{}", step_count),
            AgentState::Idle => format!("idle:{}", step_count),
        };

        if self.last_state_signature != sig {
            self.last_state_signature = sig;
            self.is_acknowledged = false; // Reset ack on any state transition
        }
    }

    pub fn acknowledge(&mut self) {
        self.is_acknowledged = true;
    }

    /// Returns true if this component should actively pulse/blink
    pub fn should_blink(&self, state: &AgentState) -> bool {
        matches!(state, AgentState::WaitingForInput { .. }) && !self.is_acknowledged
    }
}

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
    pub attention: AttentionState,
}

pub struct SessionHub {
    pub sessions: Vec<ActiveSession>,
    pub rx: Receiver<SessionEvent>,
    pub tx: Sender<SessionEvent>,
    pub selected_tab_idx: usize, // 0 = Windows, 1 = WSL2
}

impl SessionHub {
    pub fn new() -> Self {
        let (tx, rx) = channel::<SessionEvent>();

        Self {
            sessions: Vec::new(),
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
                existing.attention.update(&event.state, event.step_count);
                existing.state = event.state;
                existing.status_text = event.status_text;
                existing.step_count = event.step_count;
                existing.metadata = event.metadata;
                existing.last_updated = Instant::now();
            } else {
                let mut attention = AttentionState::new();
                attention.update(&event.state, event.step_count);

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
                    attention,
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

    /// Checks if any session in the given list requires user input (either blinking or acknowledged)
    pub fn has_waiting_input(sessions: &[&ActiveSession]) -> bool {
        sessions.iter().any(|s| matches!(s.state, AgentState::WaitingForInput { .. }))
    }

    /// Checks if any session in the given list has an unacknowledged active blinking alert
    pub fn has_unacknowledged_input(sessions: &[&ActiveSession]) -> bool {
        sessions.iter().any(|s| s.attention.should_blink(&s.state))
    }

    /// Acknowledges a single session
    pub fn acknowledge_session(&mut self, session_id: &str) {
        if let Some(session) = self.sessions.iter_mut().find(|s| s.session_id == session_id) {
            session.attention.acknowledge();
        }
    }

    /// Acknowledges all sessions in a given tab
    pub fn acknowledge_tab(&mut self, tab_idx: usize) {
        for s in self.sessions.iter_mut() {
            let is_win = s.metadata.host.eq_ignore_ascii_case("windows") || s.session_id.starts_with("win-");
            if (tab_idx == 0 && is_win) || (tab_idx == 1 && !is_win) {
                s.attention.acknowledge();
            }
        }
    }
}
