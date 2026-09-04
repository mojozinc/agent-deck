#![allow(dead_code, unused_imports)]

use agent_deck_core::{AgentState, SessionEvent, SessionMetadata};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

#[path = "../../crates/agent-deck-ui/src/hub.rs"]
pub mod hub;

pub use hub::{ActiveSession, AttentionState, CustomTitlesStorage, DynamicCategory, SessionHub, UserAction};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(1);

/// RAII Temporary Directory for test isolation
pub struct TestTempDir {
    pub path: PathBuf,
}

impl TestTempDir {
    pub fn new(prefix: &str) -> Self {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let dir = std::env::temp_dir().join(format!("agent_deck_{}_{}_{}", prefix, timestamp, id));
        fs::create_dir_all(&dir).expect("Failed to create temporary test directory");
        Self { path: dir }
    }

    pub fn file_path(&self, rel: &str) -> PathBuf {
        self.path.join(rel)
    }

    pub fn create_sub_dir(&self, rel: &str) -> PathBuf {
        let p = self.path.join(rel);
        fs::create_dir_all(&p).expect("Failed to create sub directory");
        p
    }
}

impl Drop for TestTempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Helper to create a test SessionHub with isolated in-memory/temp title storage
pub fn create_test_hub() -> SessionHub {
    SessionHub::new(Arc::new(RwLock::new(CustomTitlesStorage::in_memory())))
}

/// Creates a standard SessionEvent for testing
pub fn make_event(
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

/// Creates a mock Antigravity transcript directory structure
pub fn setup_antigravity_session_dir(base_dir: &Path, session_id: &str) -> (PathBuf, PathBuf) {
    let session_dir = base_dir.join(session_id);
    let log_dir = session_dir.join(".system_generated").join("logs");
    fs::create_dir_all(&log_dir).expect("create log dir");
    let transcript_file = log_dir.join("transcript.jsonl");
    (session_dir, transcript_file)
}

/// Writes a JSON line into a file
pub fn append_json_line(path: &Path, value: &serde_json::Value) {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open file for append");
    let json_str = serde_json::to_string(value).expect("serialize json");
    writeln!(file, "{}", json_str).expect("write jsonl line");
    file.flush().expect("flush file");
}

/// Writes partial raw bytes into a file (without trailing newline)
pub fn append_raw_bytes(path: &Path, bytes: &[u8]) {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open file for append raw");
    file.write_all(bytes).expect("write raw bytes");
    file.flush().expect("flush raw bytes");
}

/// Layout math helpers reflecting AgentDeck UI formulas
pub struct LayoutFormulas;

impl LayoutFormulas {
    pub const MIN_FONT_SCALE: f32 = 0.85;
    pub const MAX_FONT_SCALE: f32 = 1.60;

    pub fn clamp_font_scale(scale: f32) -> f32 {
        scale.clamp(Self::MIN_FONT_SCALE, Self::MAX_FONT_SCALE)
    }

    pub fn normal_row_height(scale: f32) -> f32 {
        52.0 * scale.min(1.3)
    }

    pub fn edit_row_height(scale: f32) -> f32 {
        74.0 * scale.min(1.3)
    }

    pub fn badge_font_size(scale: f32) -> f32 {
        10.5 * scale
    }

    pub fn status_font_size(scale: f32) -> f32 {
        11.5 * scale
    }

    pub fn button_font_size(scale: f32) -> f32 {
        9.0 * scale
    }

    pub fn marquee_area_height(scale: f32) -> f32 {
        18.0 * scale
    }

    pub fn marquee_advance(current_offset: f32, dt: f32) -> f32 {
        current_offset + dt * 38.0
    }

    pub fn marquee_modulo_offset(offset: f32, text_len: usize, scale: f32) -> f32 {
        let display_text_len = text_len + 6; // "   {}   "
        let char_w = 7.0 * scale;
        let total_text_width = display_text_len as f32 * char_w;
        let wrap_width = total_text_width + 40.0;
        offset % wrap_width
    }

    pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
        a + (b - a) * t.clamp(0.0, 1.0)
    }

    pub fn vu_update_active(bar: f32, i: usize, pulse_phase: f32, dt: f32) -> f32 {
        let wave = ((pulse_phase * 2.8 + i as f32 * 0.6).sin() * 0.5 + 0.5)
            * ((pulse_phase * 1.1 + (8 - i) as f32 * 0.4).cos() * 0.4 + 0.6);
        Self::lerp(bar, wave, dt * 12.0)
    }

    pub fn vu_update_decay(bar: f32, target: f32, dt: f32) -> f32 {
        Self::lerp(bar, target, dt * 6.0)
    }

    pub fn led_breathe_intensity(pulse_phase: f32) -> f32 {
        let breathe = (pulse_phase * 1.5).sin() * 0.35 + 0.65;
        breathe.clamp(0.2, 1.0)
    }
}
