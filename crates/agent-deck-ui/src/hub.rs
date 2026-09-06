#![allow(dead_code)]

use agent_deck_core::{AgentState, SessionEvent, SessionMetadata};
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, RwLock};
use std::time::Instant;

#[derive(Clone, Debug, PartialEq)]
pub struct DynamicCategory {
    pub id: String,
    pub label: String,
    pub is_permanent: bool, // true for Windows
    pub session_count: usize,
}

/// Single-pass category summary computing count, waiting input, and unacknowledged alerts (F9)
#[derive(Clone, Debug, PartialEq)]
pub struct CategorySummary {
    pub category: DynamicCategory,
    pub id: String,
    pub label: String,
    pub is_permanent: bool,
    pub session_count: usize,
    pub has_waiting_input: bool,
    pub has_unacknowledged: bool,
}

impl CategorySummary {
    pub fn new(
        id: String,
        label: String,
        is_permanent: bool,
        session_count: usize,
        has_waiting_input: bool,
        has_unacknowledged: bool,
    ) -> Self {
        Self {
            category: DynamicCategory {
                id: id.clone(),
                label: label.clone(),
                is_permanent,
                session_count,
            },
            id,
            label,
            is_permanent,
            session_count,
            has_waiting_input,
            has_unacknowledged,
        }
    }
}

/// Persistent dismissal tracking mapping session_id -> dismissed_step_count.
/// Eliminates the resurrection bug while providing .contains() compatibility for test suites.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DismissedSessions(pub HashMap<String, u32>);

impl DismissedSessions {
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    pub fn contains<Q>(&self, key: &Q) -> bool
    where
        String: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.0.contains_key(key)
    }
}

impl Deref for DismissedSessions {
    type Target = HashMap<String, u32>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for DismissedSessions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<'a> IntoIterator for &'a DismissedSessions {
    type Item = (&'a String, &'a u32);
    type IntoIter = std::collections::hash_map::Iter<'a, String, u32>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'a> IntoIterator for &'a mut DismissedSessions {
    type Item = (&'a String, &'a mut u32);
    type IntoIter = std::collections::hash_map::IterMut<'a, String, u32>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
    }
}

impl IntoIterator for DismissedSessions {
    type Item = (String, u32);
    type IntoIter = std::collections::hash_map::IntoIter<String, u32>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl From<HashMap<String, u32>> for DismissedSessions {
    fn from(map: HashMap<String, u32>) -> Self {
        Self(map)
    }
}

impl From<DismissedSessions> for HashMap<String, u32> {
    fn from(d: DismissedSessions) -> Self {
        d.0
    }
}

impl PartialEq<HashMap<String, u32>> for DismissedSessions {
    fn eq(&self, other: &HashMap<String, u32>) -> bool {
        &self.0 == other
    }
}

#[derive(Clone, Debug)]
pub enum UserAction {
    Dismiss(String),
    Rename(String, String),
    Select(String),
    AcknowledgeCategory(String),
    AcknowledgeAll,
}

pub type SessionAttention = AttentionState;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AttentionState {
    pub is_unacknowledged: bool,
    pub triggered_at: Option<Instant>,
    pub last_state_signature: String,
}

impl AttentionState {
    pub fn new() -> Self {
        Self {
            is_unacknowledged: false,
            triggered_at: None,
            last_state_signature: String::new(),
        }
    }

    /// Updates the attention tracker with a new state.
    pub fn update(&mut self, state: &AgentState, step_count: u32) {
        let sig = match state {
            AgentState::WaitingForApproval { name, summary } => format!("approval:{}:{}:{}", step_count, name, summary),
            AgentState::WaitingForInput { prompt_preview } => format!("waiting:{}:{}", step_count, prompt_preview),
            AgentState::RunningTool { name, .. } => format!("tool:{}:{}", step_count, name),
            AgentState::Thinking => format!("thinking:{}", step_count),
            AgentState::Error { message } => format!("error:{}:{}", step_count, message),
            AgentState::Finished => format!("finished:{}", step_count),
            AgentState::Idle => format!("idle:{}", step_count),
            AgentState::Exited => format!("exited:{}", step_count),
        };

        if self.last_state_signature != sig {
            let is_attention_state = matches!(state, AgentState::WaitingForInput { .. } | AgentState::WaitingForApproval { .. });

            self.last_state_signature = sig;

            if is_attention_state {
                self.is_unacknowledged = true;
                self.triggered_at = Some(Instant::now());
            } else {
                self.is_unacknowledged = false;
                self.triggered_at = None;
            }
        }
    }

    /// Convenience updater accepting boolean active indicator
    pub fn update_active(&mut self, state: &AgentState, is_active: bool) {
        let step = if is_active { 1 } else { 0 };
        self.update(state, step);
    }

    pub fn acknowledge(&mut self) {
        self.is_unacknowledged = false;
        self.triggered_at = None;
    }

    /// Returns true if actively pulsating smoothly (stops automatically after 4 seconds)
    pub fn is_pulsating(&self, state: &AgentState) -> bool {
        if !matches!(state, AgentState::WaitingForInput { .. } | AgentState::WaitingForApproval { .. }) || !self.is_unacknowledged {
            return false;
        }

        if let Some(triggered) = self.triggered_at {
            if triggered.elapsed().as_secs_f32() > 4.0 {
                return false; // Auto-stop pulsating after 4 seconds!
            }
        }
        true
    }
}

/// Persists user-defined custom friendly session names to disk
#[derive(Clone, Debug)]
pub struct CustomTitlesStorage {
    file_path: Option<PathBuf>,
    pub titles: HashMap<String, String>,
}

impl CustomTitlesStorage {
    pub fn load() -> Self {
        #[cfg(test)]
        {
            return Self::in_memory();
        }
        #[cfg(not(test))]
        {
            if std::env::var("AGENT_DECK_IN_MEMORY").is_ok() {
                return Self::in_memory();
            }
            if let Ok(custom_path) = std::env::var("AGENT_DECK_TITLES_PATH") {
                return Self::with_path(PathBuf::from(custom_path));
            }
            let dir = if let Ok(appdata) = std::env::var("APPDATA") {
                PathBuf::from(appdata).join("agent-deck")
            } else {
                let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".to_string());
                PathBuf::from(home).join(".agent-deck")
            };
            let file_path = dir.join("session_titles.json");
            Self::with_path(file_path)
        }
    }

    /// Creates an isolated in-memory title store that never writes to disk
    pub fn in_memory() -> Self {
        Self {
            file_path: None,
            titles: HashMap::new(),
        }
    }

    /// Creates a title store bound to a specific file path for test isolation
    pub fn with_path(path: PathBuf) -> Self {
        let titles = if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                serde_json::from_str::<HashMap<String, String>>(&content).unwrap_or_default()
            } else {
                HashMap::new()
            }
        } else {
            HashMap::new()
        };

        Self {
            file_path: Some(path),
            titles,
        }
    }

    pub fn file_path(&self) -> Option<&PathBuf> {
        self.file_path.as_ref()
    }

    pub fn is_in_memory(&self) -> bool {
        self.file_path.is_none()
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

    pub fn save(&self) {
        if let Some(ref path) = self.file_path {
            if let Ok(json) = serde_json::to_string_pretty(&self.titles) {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(path, json);
            }
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

impl ActiveSession {
    /// Returns true if this session belongs to the Windows category
    pub fn is_windows_session(&self) -> bool {
        self.metadata.host.eq_ignore_ascii_case("windows") || self.session_id.starts_with("win-")
    }

    /// Normalized category label for the host (e.g. "Ubuntu" from "wsl:Ubuntu", "WSL2" for empty host)
    pub fn host_category_label(&self) -> &str {
        if let Some(stripped) = self.metadata.host.strip_prefix("wsl:") {
            stripped
        } else if self.metadata.host.is_empty() {
            "WSL2"
        } else {
            self.metadata.host.as_str()
        }
    }

    /// Determines if this session belongs to the specified category ID
    pub fn matches_category(&self, cat_id: &str) -> bool {
        if cat_id.eq_ignore_ascii_case("windows") {
            self.is_windows_session()
        } else if let Some(target_host) = cat_id.strip_prefix("host:") {
            !self.is_windows_session() && self.host_category_label().eq_ignore_ascii_case(target_host)
        } else {
            !self.is_windows_session() && self.host_category_label().eq_ignore_ascii_case(cat_id)
        }
    }

    /// Returns true if this session has seen no updates for > 15 minutes
    pub fn is_stale(&self) -> bool {
        self.last_updated.elapsed().as_secs() > 15 * 60
    }

    /// Priority ranking for sorting:
    /// 1. Waiting for Permission / Approval (Top priority)
    /// 2. Thinking / Processing / Running tools
    /// 3. Waiting for prompt / Turn finished
    /// 4. Idle / Finished
    /// 99. Stale (> 15m inactive)
    pub fn sort_priority(&self) -> u8 {
        if self.is_stale() {
            return 99;
        }

        match &self.state {
            AgentState::WaitingForApproval { .. } => 1,
            AgentState::Thinking => 2,
            AgentState::RunningTool { .. } => 2,
            AgentState::Error { .. } => 2,
            AgentState::WaitingForInput { .. } => 3,
            AgentState::Idle => 4,
            AgentState::Finished => 5,
            AgentState::Exited => 99,
        }
    }
    /// Returns true if this session is actively working (Thinking or RunningTool)
    pub fn is_active(&self) -> bool {
        matches!(self.state, AgentState::Thinking | AgentState::RunningTool { .. })
    }

    /// In-place animation updates: advances marquee offset and computes VU ballistics without session cloning
    pub fn update_animations(&mut self, dt: f32, pulse_phase: f32) {
        let is_active = self.is_active();
        let is_stale = self.is_stale();

        if is_active && !is_stale {
            self.marquee_offset += dt * 38.0;
        } else {
            self.marquee_offset = 0.0;
        }

        // VU meter ballistics: update all 8 bands in-place
        for (i, bar) in self.vu_levels.iter_mut().enumerate() {
            if is_active && !is_stale {
                let wave = ((pulse_phase * 2.8 + i as f32 * 0.6).sin() * 0.5 + 0.5)
                    * ((pulse_phase * 1.1 + (8 - i) as f32 * 0.4).cos() * 0.4 + 0.6);
                let t = (dt * 12.0).clamp(0.0, 1.0);
                *bar += (wave - *bar) * t;
            } else {
                let t = (dt * 6.0).clamp(0.0, 1.0);
                *bar += (0.0 - *bar) * t;
            }
            *bar = bar.clamp(0.0, 1.0);
        }
    }
}

pub struct SessionHub {
    pub sessions: Vec<ActiveSession>,
    pub rx: Receiver<SessionEvent>,
    pub tx: Sender<SessionEvent>,
    pub selected_tab_idx: usize,
    pub custom_titles: Arc<RwLock<CustomTitlesStorage>>,
    pub connected_bridges: HashMap<String, Instant>, // Distro -> Last Heartbeat
    pub last_bridge_connected_at: Option<Instant>,
    pub dismissed_sessions: DismissedSessions,
    pub cached_summary: Option<Vec<CategorySummary>>,
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
            dismissed_sessions: DismissedSessions::new(),
            cached_summary: None,
        }
    }

    pub fn sender(&self) -> Sender<SessionEvent> {
        self.tx.clone()
    }

    /// Updates animations across all active sessions in-place without cloning
    pub fn update_animations(&mut self, dt: f32, pulse_phase: f32) {
        for session in &mut self.sessions {
            session.update_animations(dt, pulse_phase);
        }
    }

    /// Clamps selected_tab_idx within active categories bounds to prevent out-of-bounds panics
    pub fn clamp_selected_tab(&mut self) {
        let cat_len = self.active_categories().len();
        if cat_len == 0 {
            self.selected_tab_idx = 0;
        } else if self.selected_tab_idx >= cat_len {
            self.selected_tab_idx = cat_len - 1;
        }
    }

    /// Ingests all pending events from active stream adapters
    pub fn poll_events(&mut self) {
        let titles_arc = Arc::clone(&self.custom_titles);
        let titles_guard = titles_arc.read().ok();

        while let Ok(event) = self.rx.try_recv() {
            // Check for bridge handshake / heartbeat events
            if event.agent_type == "Bridge" {
                let distro = event
                    .metadata
                    .host
                    .strip_prefix("wsl:")
                    .unwrap_or(&event.metadata.host)
                    .to_string();

                let is_first_time = self
                    .connected_bridges
                    .get(&distro)
                    .map(|last| last.elapsed().as_secs() > 10)
                    .unwrap_or(true);

                if is_first_time {
                    self.last_bridge_connected_at = Some(Instant::now());
                }

                self.connected_bridges.insert(distro, Instant::now());
                continue;
            }

            // Immediately clean up exited sessions from UI state
            if event.state == AgentState::Exited {
                let existing_step = self
                    .sessions
                    .iter()
                    .find(|s| s.session_id == event.session_id)
                    .map(|s| s.step_count)
                    .unwrap_or(0);
                let prev_dismissed = self.dismissed_sessions.get(&event.session_id).copied().unwrap_or(0);
                let max_step = event.step_count.max(existing_step).max(prev_dismissed);
                self.sessions.retain(|s| s.session_id != event.session_id);
                self.dismissed_sessions.insert(event.session_id, max_step);
                continue;
            }

            // Ignore dismissed sessions unless brand new active steps occur (fixes resurrection bug)
            if let Some(&dismissed_step) = self.dismissed_sessions.get(&event.session_id) {
                if event.step_count <= dismissed_step {
                    continue;
                }
                self.dismissed_sessions.remove(&event.session_id);
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
                existing.step_count = existing.step_count.max(event.step_count);
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

        self.invalidate_category_cache();
    }

    /// Two-Pass Action Queue: Applies user actions safely outside render loops.
    /// Guarantees index out-of-bounds safety, race resilience, and case-insensitive matching.
    pub fn apply_actions(&mut self, actions: Vec<UserAction>) {
        if actions.is_empty() {
            return;
        }

        for action in actions {
            match action {
                UserAction::Dismiss(session_id) => {
                    let session_step = self
                        .sessions
                        .iter()
                        .find(|s| s.session_id == session_id)
                        .map(|s| s.step_count)
                        .unwrap_or(0);
                    let prev_dismissed = self.dismissed_sessions.get(&session_id).copied().unwrap_or(0);
                    let step_count = session_step.max(prev_dismissed);
                    self.dismissed_sessions.insert(session_id.clone(), step_count);
                    self.sessions.retain(|s| s.session_id != session_id);
                }
                UserAction::Rename(session_id, new_name) => {
                    let trimmed = new_name.trim();
                    if let Ok(mut storage) = self.custom_titles.write() {
                        storage.set_title(&session_id, trimmed);
                    }
                    if let Some(session) = self.sessions.iter_mut().find(|s| s.session_id == session_id) {
                        if !trimmed.is_empty() {
                            session.display_name = trimmed.to_string();
                        }
                    }
                }
                UserAction::Select(session_id) => {
                    if let Some(s) = self.sessions.iter_mut().find(|s| s.session_id == session_id) {
                        s.attention.acknowledge();
                    }
                }
                UserAction::AcknowledgeCategory(cat_id) => {
                    for s in self.sessions.iter_mut() {
                        if s.matches_category(&cat_id) {
                            s.attention.acknowledge();
                        }
                    }
                }
                UserAction::AcknowledgeAll => {
                    for s in self.sessions.iter_mut() {
                        s.attention.acknowledge();
                    }
                    self.last_bridge_connected_at = None;
                }
            }
        }

        self.clamp_selected_tab();
        self.invalidate_category_cache();
    }

    /// Returns list of active WSL bridge distros
    pub fn get_active_bridges(&self) -> Vec<String> {
        self.connected_bridges
            .iter()
            .filter(|(_, last)| last.elapsed().as_secs() < 8)
            .map(|(d, _)| d.clone())
            .collect()
    }

    /// Single-pass category summary computing session counts, waiting indicators, and unacknowledged alerts (F9)
    pub fn category_summary(&self) -> Vec<CategorySummary> {
        let mut host_stats: HashMap<String, (usize, bool, bool)> = HashMap::new();

        // 1. Seed connected bridges with 0 sessions
        for (distro, last) in &self.connected_bridges {
            if last.elapsed().as_secs() < 8 {
                host_stats.entry(distro.clone()).or_insert((0, false, false));
            }
        }

        let mut win_count = 0;
        let mut win_waiting = false;
        let mut win_unack = false;

        // 2. Single O(N) pass across all live sessions
        for s in &self.sessions {
            let is_waiting = matches!(s.state, AgentState::WaitingForInput { .. } | AgentState::WaitingForApproval { .. });
            let is_unack = s.attention.is_pulsating(&s.state);

            if s.is_windows_session() {
                win_count += 1;
                if is_waiting {
                    win_waiting = true;
                }
                if is_unack {
                    win_unack = true;
                }
            } else {
                let clean_label = s.host_category_label().to_string();

                let entry = host_stats.entry(clean_label).or_insert((0, false, false));
                entry.0 += 1;
                if is_waiting {
                    entry.1 = true;
                }
                if is_unack {
                    entry.2 = true;
                }
            }
        }

        let mut summaries = Vec::new();

        // 3. Permanent Windows category tab (always first anchor)
        summaries.push(CategorySummary::new(
            "windows".to_string(),
            "Windows".to_string(),
            true,
            win_count,
            win_waiting,
            win_unack,
        ));

        // 4. Discovered host distros sorted alphabetically
        let mut sorted_hosts: Vec<(String, (usize, bool, bool))> = host_stats.into_iter().collect();
        sorted_hosts.sort_by(|a, b| a.0.cmp(&b.0));

        for (host_label, (count, waiting, unack)) in sorted_hosts {
            let cat_id = format!("host:{}", host_label);
            summaries.push(CategorySummary::new(
                cat_id,
                host_label,
                false,
                count,
                waiting,
                unack,
            ));
        }

        summaries
    }

    /// Dynamically computes active category tabs (delegates to single-pass category_summary)
    pub fn active_categories(&self) -> Vec<DynamicCategory> {
        self.category_summary()
            .into_iter()
            .map(|s| s.category)
            .collect()
    }

    /// Explicitly refreshes the cached category summary
    pub fn refresh_category_cache(&mut self) {
        self.cached_summary = Some(self.category_summary());
    }

    /// Returns the cached category summary, or computes and caches it if absent
    pub fn get_or_compute_category_summary(&mut self) -> &[CategorySummary] {
        if self.cached_summary.is_none() {
            self.cached_summary = Some(self.category_summary());
        }
        self.cached_summary.as_deref().unwrap()
    }

    /// Returns the currently cached category summary if present
    pub fn cached_categories(&self) -> Option<&[CategorySummary]> {
        self.cached_summary.as_deref()
    }

    /// Clears the category cache
    pub fn invalidate_category_cache(&mut self) {
        self.cached_summary = None;
    }

    /// Returns sessions belonging to a specific category, sorted by status priority:
    /// Waiting for Permission > Thinking / Processing > Waiting for Prompt > Stale
    pub fn sessions_for_category<'a>(&'a self, cat: &DynamicCategory) -> Vec<&'a ActiveSession> {
        let mut list: Vec<&'a ActiveSession> = self
            .sessions
            .iter()
            .filter(|s| s.matches_category(&cat.id))
            .collect();

        // Sort by priority rank (Permission > Thinking/Running > Prompt > Stale)
        // Tie-breaker: Most recent update first
        list.sort_by(|a, b| {
            let prio_a = a.sort_priority();
            let prio_b = b.sort_priority();
            if prio_a != prio_b {
                prio_a.cmp(&prio_b)
            } else {
                b.last_updated.cmp(&a.last_updated)
            }
        });

        list
    }

    pub fn sessions_for_summary<'a>(&'a self, summary: &CategorySummary) -> Vec<&'a ActiveSession> {
        self.sessions_for_category(&summary.category)
    }

    /// Checks if any session in the given list requires user input or approval
    pub fn has_waiting_input(sessions: &[&ActiveSession]) -> bool {
        sessions.iter().any(|s| matches!(s.state, AgentState::WaitingForInput { .. } | AgentState::WaitingForApproval { .. }))
    }

    /// Checks if any session in the given list has an unacknowledged active alert that is still pulsating
    pub fn has_unacknowledged_input(sessions: &[&ActiveSession]) -> bool {
        sessions.iter().any(|s| s.attention.is_pulsating(&s.state))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(1);

    fn make_test_event(
        session_id: &str,
        display_name: &str,
        agent_type: &str,
        state: AgentState,
        status_text: &str,
        step_count: u32,
        host: &str,
    ) -> SessionEvent {
        SessionEvent::new(
            session_id,
            display_name,
            agent_type,
            state,
            status_text,
            step_count,
            SessionMetadata {
                host: host.to_string(),
                tmux_session: None,
                tmux_window: None,
                tmux_pane: None,
                cwd: None,
                pid: None,
                agent_type: None,
            },
        )
    }

    fn test_hub() -> SessionHub {
        SessionHub::new(Arc::new(RwLock::new(CustomTitlesStorage::in_memory())))
    }

    // =========================================================================
    // Task 1: Dismissal Resurrection Fix & Map Operations
    // =========================================================================

    #[test]
    fn test_dismiss_records_step_count_and_removes_from_sessions() {
        let mut hub = test_hub();
        hub.sender()
            .send(make_test_event("s1", "Session 1", "Gemini", AgentState::Thinking, "Thinking", 10, "Windows"))
            .unwrap();
        hub.poll_events();
        assert_eq!(hub.sessions.len(), 1);

        hub.apply_actions(vec![UserAction::Dismiss("s1".to_string())]);
        assert_eq!(hub.sessions.len(), 0);
        assert!(hub.dismissed_sessions.contains("s1"));
        assert_eq!(hub.dismissed_sessions.get("s1"), Some(&10));
    }

    #[test]
    fn test_dismissed_session_ignores_stale_or_equal_step_events() {
        let mut hub = test_hub();
        hub.sender()
            .send(make_test_event("s1", "Session 1", "Gemini", AgentState::Thinking, "Thinking", 10, "Windows"))
            .unwrap();
        hub.poll_events();

        hub.apply_actions(vec![UserAction::Dismiss("s1".to_string())]);
        assert_eq!(hub.sessions.len(), 0);

        // Retransmission at exact dismissed step count (10) must be ignored
        hub.sender()
            .send(make_test_event("s1", "Session 1", "Gemini", AgentState::Thinking, "Thinking", 10, "Windows"))
            .unwrap();
        hub.poll_events();
        assert_eq!(hub.sessions.len(), 0, "Session must remain dismissed on stale retransmission");
        assert!(hub.dismissed_sessions.contains("s1"));

        // Retransmission at earlier step count (5) must also be ignored
        hub.sender()
            .send(make_test_event("s1", "Session 1", "Gemini", AgentState::Thinking, "Thinking", 5, "Windows"))
            .unwrap();
        hub.poll_events();
        assert_eq!(hub.sessions.len(), 0, "Session must remain dismissed on older step count");
        assert!(hub.dismissed_sessions.contains("s1"));
    }

    #[test]
    fn test_dismissed_session_resurrects_on_newer_step_event() {
        let mut hub = test_hub();
        hub.sender()
            .send(make_test_event("s1", "Session 1", "Gemini", AgentState::Thinking, "Thinking", 10, "Windows"))
            .unwrap();
        hub.poll_events();

        hub.apply_actions(vec![UserAction::Dismiss("s1".to_string())]);
        assert_eq!(hub.sessions.len(), 0);

        // New active turn event (step 11 > 10) must resurrect session
        hub.sender()
            .send(make_test_event("s1", "Session 1", "Gemini", AgentState::Thinking, "New Turn", 11, "Windows"))
            .unwrap();
        hub.poll_events();

        assert_eq!(hub.sessions.len(), 1, "Session with higher step count must resurrect");
        assert_eq!(hub.sessions[0].step_count, 11);
        assert!(!hub.dismissed_sessions.contains("s1"));
    }

    #[test]
    fn test_dismiss_nonexistent_session_records_zero() {
        let mut hub = test_hub();
        hub.apply_actions(vec![UserAction::Dismiss("ghost".to_string())]);
        assert!(hub.dismissed_sessions.contains("ghost"));
        assert_eq!(hub.dismissed_sessions.get("ghost"), Some(&0));
    }

    #[test]
    fn test_duplicate_dismiss_preserves_highest_step() {
        let mut hub = test_hub();
        hub.sender()
            .send(make_test_event("s1", "S1", "Gemini", AgentState::Thinking, "T", 42, "Windows"))
            .unwrap();
        hub.poll_events();

        hub.apply_actions(vec![
            UserAction::Dismiss("s1".to_string()),
            UserAction::Dismiss("s1".to_string()),
        ]);
        assert_eq!(hub.dismissed_sessions.get("s1"), Some(&42));
    }

    #[test]
    fn test_dismissed_sessions_map_operations() {
        let mut map = DismissedSessions::new();
        assert!(map.is_empty());
        assert_eq!(map.len(), 0);

        map.insert("a".to_string(), 1);
        map.insert("b".to_string(), 2);
        assert_eq!(map.len(), 2);
        assert!(map.contains("a"));
        assert!(map.contains_key("b"));
        assert!(!map.contains("c"));

        let mut collected: Vec<(String, u32)> = (&map).into_iter().map(|(k, v)| (k.clone(), *v)).collect();
        collected.sort();
        assert_eq!(collected, vec![("a".to_string(), 1), ("b".to_string(), 2)]);

        map.remove("a");
        assert_eq!(map.len(), 1);
        assert!(!map.contains("a"));
    }

    // =========================================================================
    // Task 2: Action Queue Safety
    // =========================================================================

    #[test]
    fn test_rapid_action_queue_safety_mixed_actions() {
        let mut hub = test_hub();
        for i in 1..=5 {
            hub.sender()
                .send(make_test_event(&format!("s{}", i), &format!("Session {}", i), "Gemini", AgentState::Idle, "Idle", 1, "Windows"))
                .unwrap();
        }
        hub.poll_events();
        assert_eq!(hub.sessions.len(), 5);

        // Rapid interleaved actions: Dismiss, Rename, Select, Acknowledge
        let actions = vec![
            UserAction::Dismiss("s1".to_string()),
            UserAction::Rename("s2".to_string(), "Renamed S2".to_string()),
            UserAction::Select("s3".to_string()),
            UserAction::Dismiss("s4".to_string()),
            UserAction::Rename("s4".to_string(), "Ghost Rename".to_string()), // Target already dismissed
            UserAction::Select("s1".to_string()),                             // Target already dismissed
            UserAction::AcknowledgeCategory("windows".to_string()),
        ];
        hub.apply_actions(actions);

        assert_eq!(hub.sessions.len(), 3);
        let remaining_ids: Vec<String> = hub.sessions.iter().map(|s| s.session_id.clone()).collect();
        assert_eq!(remaining_ids, vec!["s2", "s3", "s5"]);

        let s2 = hub.sessions.iter().find(|s| s.session_id == "s2").unwrap();
        assert_eq!(s2.display_name, "Renamed S2");
    }

    #[test]
    fn test_action_queue_unknown_sessions_no_panic() {
        let mut hub = test_hub();
        hub.apply_actions(vec![
            UserAction::Select("non-existent".to_string()),
            UserAction::Rename("non-existent".to_string(), "New Name".to_string()),
            UserAction::AcknowledgeCategory("non-existent-cat".to_string()),
            UserAction::AcknowledgeAll,
        ]);
        assert_eq!(hub.sessions.len(), 0);
    }

    #[test]
    fn test_acknowledge_category_case_insensitive() {
        let mut hub = test_hub();
        hub.sender()
            .send(make_test_event("win-1", "Win1", "Gemini", AgentState::WaitingForInput { prompt_preview: "P1".into() }, "W", 1, "windows"))
            .unwrap();
        hub.sender()
            .send(make_test_event("wsl-1", "Wsl1", "Gemini", AgentState::WaitingForApproval { name: "t".into(), summary: "s".into() }, "W", 1, "wsl:Ubuntu"))
            .unwrap();
        hub.poll_events();

        assert!(hub.sessions[0].attention.is_unacknowledged);
        assert!(hub.sessions[1].attention.is_unacknowledged);

        // Case-insensitive uppercase "WINDOWS"
        hub.apply_actions(vec![UserAction::AcknowledgeCategory("WINDOWS".to_string())]);
        assert!(!hub.sessions[0].attention.is_unacknowledged, "Windows session must be acknowledged");
        assert!(hub.sessions[1].attention.is_unacknowledged, "WSL session must remain unacknowledged");

        // Case-insensitive "host:ubuntu"
        hub.apply_actions(vec![UserAction::AcknowledgeCategory("host:ubuntu".to_string())]);
        assert!(!hub.sessions[1].attention.is_unacknowledged, "Ubuntu session must be acknowledged");
    }

    #[test]
    fn test_clamp_selected_tab_after_dismissals() {
        let mut hub = test_hub();
        hub.sender()
            .send(make_test_event("wsl-1", "Wsl", "Gemini", AgentState::Idle, "I", 1, "wsl:Ubuntu"))
            .unwrap();
        hub.poll_events();

        // 2 categories: Windows (0) and host:Ubuntu (1)
        assert_eq!(hub.active_categories().len(), 2);
        hub.selected_tab_idx = 1;

        // Dismiss the only WSL session
        hub.apply_actions(vec![UserAction::Dismiss("wsl-1".to_string())]);
        assert_eq!(hub.active_categories().len(), 1);
        assert_eq!(hub.selected_tab_idx, 0, "Selected tab must be safely clamped");
    }

    // =========================================================================
    // Task 3: Startup Attention Blindspot Fix
    // =========================================================================

    #[test]
    fn test_startup_attention_blindspot_waiting_for_input() {
        let mut attention = AttentionState::new();
        assert!(attention.last_state_signature.is_empty());

        let state = AgentState::WaitingForInput {
            prompt_preview: "Initial user prompt".to_string(),
        };
        attention.update(&state, 1);

        assert!(
            attention.is_unacknowledged,
            "Startup session in WaitingForInput MUST be unacknowledged (no blindspot)"
        );
        assert!(attention.triggered_at.is_some());
        assert!(attention.is_pulsating(&state));
    }

    #[test]
    fn test_startup_attention_blindspot_waiting_for_approval() {
        let mut attention = AttentionState::new();
        assert!(attention.last_state_signature.is_empty());

        let state = AgentState::WaitingForApproval {
            name: "execute_command".to_string(),
            summary: "rm -rf target".to_string(),
        };
        attention.update(&state, 1);

        assert!(
            attention.is_unacknowledged,
            "Startup session in WaitingForApproval MUST be unacknowledged (no blindspot)"
        );
        assert!(attention.triggered_at.is_some());
        assert!(attention.is_pulsating(&state));
    }

    #[test]
    fn test_startup_attention_thinking_state_no_unacknowledged() {
        let mut attention = AttentionState::new();
        let state = AgentState::Thinking;
        attention.update(&state, 1);

        assert!(!attention.is_unacknowledged);
        assert!(attention.triggered_at.is_none());
        assert!(!attention.is_pulsating(&state));
    }

    #[test]
    fn test_attention_no_retrigger_on_identical_signature() {
        let mut attention = AttentionState::new();
        let state = AgentState::WaitingForInput { prompt_preview: "Query".into() };
        attention.update(&state, 1);
        assert!(attention.is_unacknowledged);

        attention.acknowledge();
        assert!(!attention.is_unacknowledged);

        // Same state and step count does NOT retrigger
        attention.update(&state, 1);
        assert!(!attention.is_unacknowledged);
    }

    #[test]
    fn test_attention_retrigger_on_new_turn_step_count() {
        let mut attention = AttentionState::new();
        let state = AgentState::WaitingForInput { prompt_preview: "Query".into() };
        attention.update(&state, 1);
        attention.acknowledge();

        // New step count triggers fresh attention
        attention.update(&state, 2);
        assert!(attention.is_unacknowledged);
        assert!(attention.triggered_at.is_some());
    }

    #[test]
    fn test_attention_pulse_timeout_auto_stops() {
        let mut attention = AttentionState::new();
        let state = AgentState::WaitingForInput { prompt_preview: "P".into() };
        attention.update(&state, 1);
        assert!(attention.is_pulsating(&state));

        // Advance triggered_at past 4 seconds
        attention.triggered_at = Some(Instant::now() - Duration::from_millis(4100));
        assert!(!attention.is_pulsating(&state));
    }

    // =========================================================================
    // Task 4: Custom Title Storage Test Isolation
    // =========================================================================

    #[test]
    fn test_custom_titles_in_memory_isolation() {
        let mut storage = CustomTitlesStorage::in_memory();
        assert!(storage.is_in_memory());
        assert!(storage.file_path().is_none());

        storage.set_title("sess-1", "My Friendly Worker");
        assert_eq!(storage.get_title("sess-1"), Some("My Friendly Worker".to_string()));

        // Confirm title persists in memory
        assert_eq!(storage.titles.len(), 1);
    }

    #[test]
    fn test_custom_titles_with_path_persistence() {
        let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let temp_file = std::env::temp_dir().join(format!("agent_deck_test_titles_{}.json", id));
        let _ = std::fs::remove_file(&temp_file);

        {
            let mut storage = CustomTitlesStorage::with_path(temp_file.clone());
            assert!(!storage.is_in_memory());
            storage.set_title("s1", "Persisted Name");
        }

        assert!(temp_file.exists());

        // Reload from same path
        let loaded = CustomTitlesStorage::with_path(temp_file.clone());
        assert_eq!(loaded.get_title("s1"), Some("Persisted Name".to_string()));

        let _ = std::fs::remove_file(&temp_file);
    }

    #[test]
    fn test_custom_titles_whitespace_clears_title() {
        let mut storage = CustomTitlesStorage::in_memory();
        storage.set_title("s1", "Active Title");
        assert_eq!(storage.get_title("s1"), Some("Active Title".to_string()));

        storage.set_title("s1", "   ");
        assert_eq!(storage.get_title("s1"), None);
    }

    // =========================================================================
    // Task 5: In-Place Animation Updates
    // =========================================================================

    #[test]
    fn test_active_session_update_animations_marquee_advancement() {
        let mut session = ActiveSession {
            session_id: "s1".to_string(),
            display_name: "S1".to_string(),
            agent_type: "Gemini".to_string(),
            state: AgentState::Thinking,
            status_text: "Thinking".to_string(),
            step_count: 1,
            metadata: SessionMetadata::default(),
            last_updated: Instant::now(),
            marquee_offset: 0.0,
            vu_levels: [0.0; 8],
            attention: AttentionState::new(),
        };

        assert!(session.is_active());
        session.update_animations(0.1, 0.0);
        assert!((session.marquee_offset - 3.8).abs() < 0.001);

        session.update_animations(0.1, 0.0);
        assert!((session.marquee_offset - 7.6).abs() < 0.001);
    }

    #[test]
    fn test_idle_session_update_animations_marquee_reset() {
        let mut session = ActiveSession {
            session_id: "s1".to_string(),
            display_name: "S1".to_string(),
            agent_type: "Gemini".to_string(),
            state: AgentState::Idle,
            status_text: "Idle".to_string(),
            step_count: 1,
            metadata: SessionMetadata::default(),
            last_updated: Instant::now(),
            marquee_offset: 25.0,
            vu_levels: [0.0; 8],
            attention: AttentionState::new(),
        };

        assert!(!session.is_active());
        session.update_animations(0.1, 0.0);
        assert_eq!(session.marquee_offset, 0.0);
    }

    #[test]
    fn test_active_session_vu_levels_update() {
        let mut session = ActiveSession {
            session_id: "s1".to_string(),
            display_name: "S1".to_string(),
            agent_type: "Gemini".to_string(),
            state: AgentState::RunningTool { name: "test".into(), summary: "test".into() },
            status_text: "Running".to_string(),
            step_count: 1,
            metadata: SessionMetadata::default(),
            last_updated: Instant::now(),
            marquee_offset: 0.0,
            vu_levels: [0.0; 8],
            attention: AttentionState::new(),
        };

        session.update_animations(0.016, 1.0);
        for bar in session.vu_levels {
            assert!(bar >= 0.0 && bar <= 1.0);
        }
    }

    #[test]
    fn test_idle_session_vu_levels_decay() {
        let mut session = ActiveSession {
            session_id: "s1".to_string(),
            display_name: "S1".to_string(),
            agent_type: "Gemini".to_string(),
            state: AgentState::Idle,
            status_text: "Idle".to_string(),
            step_count: 1,
            metadata: SessionMetadata::default(),
            last_updated: Instant::now(),
            marquee_offset: 0.0,
            vu_levels: [0.8; 8],
            attention: AttentionState::new(),
        };

        session.update_animations(0.1, 0.0);
        for bar in session.vu_levels {
            assert!(bar < 0.8, "VU levels must decay towards 0.0 when idle");
        }
    }

    #[test]
    fn test_hub_update_animations_batch() {
        let mut hub = test_hub();
        hub.sender()
            .send(make_test_event("s1", "S1", "Gemini", AgentState::Thinking, "T", 1, "Windows"))
            .unwrap();
        hub.sender()
            .send(make_test_event("s2", "S2", "Gemini", AgentState::Idle, "I", 1, "Windows"))
            .unwrap();
        hub.poll_events();

        hub.update_animations(0.1, 1.0);
        assert!(hub.sessions[0].marquee_offset > 0.0);
        assert_eq!(hub.sessions[1].marquee_offset, 0.0);
    }

    // =========================================================================
    // Task 6: Cached Category Computations (F9)
    // =========================================================================

    #[test]
    fn test_category_summary_single_pass_aggregation() {
        let mut hub = test_hub();
        // Windows sessions
        hub.sender()
            .send(make_test_event("win-1", "Win1", "Gemini", AgentState::Thinking, "T", 1, "windows"))
            .unwrap();
        hub.sender()
            .send(make_test_event("win-2", "Win2", "Gemini", AgentState::WaitingForInput { prompt_preview: "P".into() }, "W", 2, "windows"))
            .unwrap();
        // Ubuntu sessions
        hub.sender()
            .send(make_test_event("u-1", "U1", "Gemini", AgentState::RunningTool { name: "x".into(), summary: "y".into() }, "R", 1, "wsl:Ubuntu"))
            .unwrap();
        hub.sender()
            .send(make_test_event("u-2", "U2", "Gemini", AgentState::WaitingForApproval { name: "a".into(), summary: "b".into() }, "A", 2, "wsl:Ubuntu"))
            .unwrap();
        // Debian session
        hub.sender()
            .send(make_test_event("d-1", "D1", "Gemini", AgentState::Idle, "I", 1, "wsl:Debian"))
            .unwrap();
        hub.poll_events();

        let summaries = hub.category_summary();
        assert_eq!(summaries.len(), 3);

        // Windows (index 0, permanent)
        assert_eq!(summaries[0].id, "windows");
        assert_eq!(summaries[0].session_count, 2);
        assert!(summaries[0].has_waiting_input);
        assert!(summaries[0].has_unacknowledged);

        // Debian (sorted alphabetically: Debian before Ubuntu)
        assert_eq!(summaries[1].id, "host:Debian");
        assert_eq!(summaries[1].session_count, 1);
        assert!(!summaries[1].has_waiting_input);
        assert!(!summaries[1].has_unacknowledged);

        // Ubuntu
        assert_eq!(summaries[2].id, "host:Ubuntu");
        assert_eq!(summaries[2].session_count, 2);
        assert!(summaries[2].has_waiting_input);
        assert!(summaries[2].has_unacknowledged);
    }

    #[test]
    fn test_category_summary_connected_bridge_without_sessions() {
        let mut hub = test_hub();
        hub.connected_bridges.insert("ArchLinux".to_string(), Instant::now());

        let summaries = hub.category_summary();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].id, "windows");
        assert_eq!(summaries[0].session_count, 0);
        assert_eq!(summaries[1].id, "host:ArchLinux");
        assert_eq!(summaries[1].session_count, 0);
    }

    #[test]
    fn test_category_cache_invalidation_and_reuse() {
        let mut hub = test_hub();
        assert!(hub.cached_categories().is_none());

        hub.refresh_category_cache();
        assert!(hub.cached_categories().is_some());
        assert_eq!(hub.cached_categories().unwrap().len(), 1);

        // Polling an event invalidates cache
        hub.sender()
            .send(make_test_event("win-1", "Win1", "Gemini", AgentState::Idle, "I", 1, "windows"))
            .unwrap();
        hub.poll_events();
        assert!(hub.cached_categories().is_none());

        // get_or_compute caches again
        let count = hub.get_or_compute_category_summary().len();
        assert_eq!(count, 1);
        assert!(hub.cached_categories().is_some());
    }

    #[test]
    fn test_empty_host_category_filtering_parity() {
        let mut hub = test_hub();
        // Session with empty metadata.host
        hub.sender()
            .send(make_test_event("session-empty-host", "Empty Host Session", "Gemini", AgentState::Idle, "I", 1, ""))
            .unwrap();
        hub.poll_events();

        let summaries = hub.category_summary();
        // Index 0 is permanent Windows (0 sessions)
        assert_eq!(summaries[0].id, "windows");
        assert_eq!(summaries[0].session_count, 0);

        // Index 1 must be WSL2 (1 session)
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[1].id, "host:WSL2");
        assert_eq!(summaries[1].label, "WSL2");
        assert_eq!(summaries[1].session_count, 1);

        // Crucial parity check: sessions_for_category must return exactly 1 session matching the summary count
        let wsl2_cat = &summaries[1].category;
        let matched_sessions = hub.sessions_for_category(wsl2_cat);
        assert_eq!(matched_sessions.len(), 1, "Category tab count must equal sessions_for_category length");
        assert_eq!(matched_sessions[0].session_id, "session-empty-host");

        // Permanent windows tab must have 0 sessions
        let win_cat = &summaries[0].category;
        assert_eq!(hub.sessions_for_category(win_cat).len(), 0);
    }

    #[test]
    fn test_empty_host_category_acknowledgement() {
        let mut hub = test_hub();
        hub.sender()
            .send(make_test_event("session-empty", "Empty Host", "Gemini", AgentState::WaitingForApproval { name: "cmd".into(), summary: "sum".into() }, "W", 1, ""))
            .unwrap();
        hub.poll_events();

        assert!(hub.sessions[0].attention.is_pulsating(&hub.sessions[0].state));

        // Acknowledge via category id "host:WSL2"
        hub.apply_actions(vec![UserAction::AcknowledgeCategory("host:WSL2".to_string())]);
        assert!(!hub.sessions[0].attention.is_pulsating(&hub.sessions[0].state));
    }

    #[test]
    fn test_exited_zero_step_does_not_downgrade_dismissed_step() {
        let mut hub = test_hub();
        // Session reaches step 10
        hub.sender()
            .send(make_test_event("sess-1", "S1", "Gemini", AgentState::Thinking, "T", 10, "Windows"))
            .unwrap();
        hub.poll_events();
        assert_eq!(hub.sessions.len(), 1);
        assert_eq!(hub.sessions[0].step_count, 10);

        // Termination event arrives with step_count = 0 (standard for process exits)
        hub.sender()
            .send(make_test_event("sess-1", "S1", "Gemini", AgentState::Exited, "Terminated", 0, "Windows"))
            .unwrap();
        hub.poll_events();

        assert_eq!(hub.sessions.len(), 0);
        assert!(hub.dismissed_sessions.contains("sess-1"));
        // Invariant: dismissed_step must remain 10, never downgraded to 0!
        assert_eq!(hub.dismissed_sessions.get("sess-1"), Some(&10));

        // Stale retransmission of turn 5 or 10 must NOT resurrect the session
        hub.sender()
            .send(make_test_event("sess-1", "S1", "Gemini", AgentState::Thinking, "T", 5, "Windows"))
            .unwrap();
        hub.poll_events();
        assert_eq!(hub.sessions.len(), 0, "Turn 5 must not resurrect session dismissed at step 10");

        hub.sender()
            .send(make_test_event("sess-1", "S1", "Gemini", AgentState::Thinking, "T", 10, "Windows"))
            .unwrap();
        hub.poll_events();
        assert_eq!(hub.sessions.len(), 0, "Turn 10 must not resurrect session dismissed at step 10");

        // Higher turn 11 DOES resurrect
        hub.sender()
            .send(make_test_event("sess-1", "S1", "Gemini", AgentState::Thinking, "T", 11, "Windows"))
            .unwrap();
        hub.poll_events();
        assert_eq!(hub.sessions.len(), 1);
        assert_eq!(hub.sessions[0].step_count, 11);
    }

    #[test]
    fn test_finished_zero_step_does_not_downgrade_session_step() {
        let mut hub = test_hub();
        hub.sender()
            .send(make_test_event("sess-2", "S2", "Gemini", AgentState::Thinking, "T", 15, "Windows"))
            .unwrap();
        hub.poll_events();
        assert_eq!(hub.sessions[0].step_count, 15);

        // Finished event arrives with step_count = 0
        hub.sender()
            .send(make_test_event("sess-2", "S2", "Gemini", AgentState::Finished, "Finished", 0, "Windows"))
            .unwrap();
        hub.poll_events();

        // Step count must remain 15
        assert_eq!(hub.sessions[0].step_count, 15);

        // User dismisses the finished session
        hub.apply_actions(vec![UserAction::Dismiss("sess-2".to_string())]);
        assert_eq!(hub.dismissed_sessions.get("sess-2"), Some(&15));

        // Retransmission at step 12 cannot resurrect
        hub.sender()
            .send(make_test_event("sess-2", "S2", "Gemini", AgentState::Thinking, "T", 12, "Windows"))
            .unwrap();
        hub.poll_events();
        assert_eq!(hub.sessions.len(), 0);
    }
}

