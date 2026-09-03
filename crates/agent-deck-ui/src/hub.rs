use agent_deck_core::{AgentState, SessionEvent, SessionMetadata};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, RwLock};
use std::time::Instant;

pub const ENABLE_BLINKING_ALERTS: bool = false; // Feature flag for blinking alerts (turned off)

#[derive(Clone, Debug, PartialEq)]
pub struct DynamicCategory {
    pub id: String,
    pub label: String,
    pub is_permanent: bool, // true for Windows
    pub session_count: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AttentionState {
    pub is_unacknowledged: bool,
    pub last_state_signature: String,
}

impl AttentionState {
    pub fn new() -> Self {
        Self {
            is_unacknowledged: false, // Default false: deck starts calm without blinking
            last_state_signature: String::new(),
        }
    }

    /// Updates the attention tracker with a new state.
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
            let is_transition_to_waiting = !self.last_state_signature.is_empty()
                && matches!(state, AgentState::WaitingForInput { .. });

            self.last_state_signature = sig;

            if is_transition_to_waiting {
                self.is_unacknowledged = true;
            } else {
                self.is_unacknowledged = false;
            }
        }
    }

    pub fn acknowledge(&mut self) {
        self.is_unacknowledged = false;
    }

    /// Returns true if this component should actively pulse/blink
    pub fn should_blink(&self, state: &AgentState) -> bool {
        ENABLE_BLINKING_ALERTS && matches!(state, AgentState::WaitingForInput { .. }) && self.is_unacknowledged
    }
}

/// Persists user-defined custom friendly session names to disk
#[derive(Clone, Debug)]
pub struct CustomTitlesStorage {
    file_path: PathBuf,
    pub titles: HashMap<String, String>,
}

impl CustomTitlesStorage {
    pub fn load() -> Self {
        let dir = if let Ok(appdata) = std::env::var("APPDATA") {
            PathBuf::from(appdata).join("agent-deck")
        } else {
            let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".agent-deck")
        };
        let _ = std::fs::create_dir_all(&dir);
        let file_path = dir.join("session_titles.json");

        let titles = if file_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&file_path) {
                serde_json::from_str::<HashMap<String, String>>(&content).unwrap_or_default()
            } else {
                HashMap::new()
            }
        } else {
            HashMap::new()
        };

        Self { file_path, titles }
    }

    pub fn set_title(&mut self, session_id: &str, custom_title: &str) {
        let trimmed = custom_title.trim();
        if trimmed.is_empty() {
            self.titles.remove(session_id);
        } else {
            self.titles.insert(session_id.to_string(), trimmed.to_string());
        }
        self.save();
    }

    pub fn get_title(&self, session_id: &str) -> Option<String> {
        self.titles.get(session_id).cloned()
    }

    fn save(&self) {
        if let Ok(json) = serde_json::to_string_pretty(&self.titles) {
            let _ = std::fs::write(&self.file_path, json);
        }
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
    pub selected_tab_idx: usize,
    pub custom_titles: Arc<RwLock<CustomTitlesStorage>>,
    pub connected_bridges: HashMap<String, Instant>, // Distro -> Last Heartbeat
    pub last_bridge_connected_at: Option<Instant>,
}

impl SessionHub {
    pub fn new(custom_titles: Arc<RwLock<CustomTitlesStorage>>) -> Self {
        let (tx, rx) = channel::<SessionEvent>();

        Self {
            sessions: Vec::new(),
            rx,
            tx,
            selected_tab_idx: 0,
            custom_titles,
            connected_bridges: HashMap::new(),
            last_bridge_connected_at: None,
        }
    }

    pub fn sender(&self) -> Sender<SessionEvent> {
        self.tx.clone()
    }

    /// Ingests all pending events from active stream adapters
    pub fn poll_events(&mut self) {
        let titles_guard = self.custom_titles.read().ok();

        while let Ok(event) = self.rx.try_recv() {
            // Check for bridge handshake / heartbeat events
            if event.agent_type == "Bridge" {
                let distro = event
                    .metadata
                    .host
                    .strip_prefix("wsl:")
                    .unwrap_or(&event.metadata.host)
                    .to_string();

                let is_new = self
                    .connected_bridges
                    .get(&distro)
                    .map(|last| last.elapsed().as_secs() > 8)
                    .unwrap_or(true);

                if is_new {
                    self.last_bridge_connected_at = Some(Instant::now());
                }

                self.connected_bridges.insert(distro, Instant::now());
                continue;
            }

            let display_name = if let Some(ref storage) = titles_guard {
                storage.get_title(&event.session_id).unwrap_or(event.display_name)
            } else {
                event.display_name
            };

            if let Some(existing) = self.sessions.iter_mut().find(|s| s.session_id == event.session_id) {
                existing.display_name = display_name;
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
                    display_name,
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

    /// Overrides a session friendly name and persists it
    pub fn set_custom_name(&mut self, session_id: &str, custom_name: &str) {
        if let Ok(mut storage) = self.custom_titles.write() {
            storage.set_title(session_id, custom_name);
        }

        if let Some(session) = self.sessions.iter_mut().find(|s| s.session_id == session_id) {
            let trimmed = custom_name.trim();
            if !trimmed.is_empty() {
                session.display_name = trimmed.to_string();
            }
        }
    }

    /// Returns list of active WSL bridge distros
    pub fn get_active_bridges(&self) -> Vec<String> {
        self.connected_bridges
            .iter()
            .filter(|(_, last)| last.elapsed().as_secs() < 8)
            .map(|(d, _)| d.clone())
            .collect()
    }

    /// Dynamically computes active category tabs.
    /// - "Windows" is always permanent.
    /// - Active connected WSL distros and discovered host categories are included.
    pub fn active_categories(&self) -> Vec<DynamicCategory> {
        let mut categories = Vec::new();

        // 1. Permanent Windows Anchor Tab
        let win_sessions: Vec<&ActiveSession> = self
            .sessions
            .iter()
            .filter(|s| s.metadata.host.eq_ignore_ascii_case("windows") || s.session_id.starts_with("win-"))
            .collect();

        categories.push(DynamicCategory {
            id: "windows".to_string(),
            label: "Windows".to_string(),
            is_permanent: true,
            session_count: win_sessions.len(),
        });

        // 2. Discover all non-Windows environment hosts from active sessions & connected bridges
        let mut discovered_hosts: HashMap<String, usize> = HashMap::new();

        // From connected bridges (even if currently 0 active sessions)
        for (distro, last) in &self.connected_bridges {
            if last.elapsed().as_secs() < 8 {
                discovered_hosts.entry(distro.clone()).or_insert(0);
            }
        }

        // From live sessions
        for s in &self.sessions {
            let host_raw = &s.metadata.host;
            let is_win = host_raw.eq_ignore_ascii_case("windows") || s.session_id.starts_with("win-");
            if !is_win {
                let clean_label = if let Some(stripped) = host_raw.strip_prefix("wsl:") {
                    stripped.to_string()
                } else if host_raw.is_empty() {
                    "WSL2".to_string()
                } else {
                    host_raw.clone()
                };

                *discovered_hosts.entry(clean_label).or_insert(0) += 1;
            }
        }

        let mut sorted_hosts: Vec<(String, usize)> = discovered_hosts.into_iter().collect();
        sorted_hosts.sort_by(|a, b| a.0.cmp(&b.0));

        for (host_label, count) in sorted_hosts {
            categories.push(DynamicCategory {
                id: format!("host:{}", host_label),
                label: host_label,
                is_permanent: false,
                session_count: count,
            });
        }

        categories
    }

    /// Returns sessions belonging to a specific category
    pub fn sessions_for_category<'a>(&'a self, cat: &DynamicCategory) -> Vec<&'a ActiveSession> {
        if cat.id == "windows" {
            self.sessions
                .iter()
                .filter(|s| s.metadata.host.eq_ignore_ascii_case("windows") || s.session_id.starts_with("win-"))
                .collect()
        } else if let Some(target_host) = cat.id.strip_prefix("host:") {
            self.sessions
                .iter()
                .filter(|s| {
                    let host_clean = s.metadata.host.strip_prefix("wsl:").unwrap_or(&s.metadata.host);
                    host_clean.eq_ignore_ascii_case(target_host)
                })
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Checks if any session in the given list requires user input
    pub fn has_waiting_input(sessions: &[&ActiveSession]) -> bool {
        sessions.iter().any(|s| matches!(s.state, AgentState::WaitingForInput { .. }))
    }

    /// Checks if any session in the given list has an unacknowledged active alert
    pub fn has_unacknowledged_input(sessions: &[&ActiveSession]) -> bool {
        sessions.iter().any(|s| s.attention.should_blink(&s.state))
    }

    /// Acknowledges all sessions in a given category
    pub fn acknowledge_category(&mut self, cat: &DynamicCategory) {
        if cat.id == "windows" {
            for s in self.sessions.iter_mut() {
                if s.metadata.host.eq_ignore_ascii_case("windows") || s.session_id.starts_with("win-") {
                    s.attention.acknowledge();
                }
            }
        } else if let Some(target_host) = cat.id.strip_prefix("host:") {
            for s in self.sessions.iter_mut() {
                let host_clean = s.metadata.host.strip_prefix("wsl:").unwrap_or(&s.metadata.host);
                if host_clean.eq_ignore_ascii_case(target_host) {
                    s.attention.acknowledge();
                }
            }
        }
    }
}
