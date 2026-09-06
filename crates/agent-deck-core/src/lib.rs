use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AgentState {
    Idle,
    Thinking,
    RunningTool { name: String, summary: String },
    WaitingForApproval { name: String, summary: String },
    WaitingForInput { prompt_preview: String },
    Error { message: String },
    Finished,
    Exited,
}

pub mod transcript;
pub use transcript::{
    extract_claude_title, extract_earliest_markdown_heading, extract_prompt_fallback,
    extract_workdir_basename, AntigravityParser, ClaudeParser, ParsedClaudeStep,
    ParsedTranscriptStep, SafeLineReader,
};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub host: String, // "Windows", "WSL2-Ubuntu", etc.
    pub tmux_session: Option<String>,
    pub tmux_window: Option<String>,
    pub tmux_pane: Option<String>,
    pub cwd: Option<String>,
    pub pid: Option<u32>,
    #[serde(default)]
    pub agent_type: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionEvent {
    pub session_id: String,
    pub display_name: String,
    pub agent_type: String, // "AGY", "Claude", etc.
    pub state: AgentState,
    pub status_text: String,
    pub step_count: u32,
    pub metadata: SessionMetadata,
    pub timestamp: DateTime<Utc>,
}

impl SessionEvent {
    pub fn new(
        session_id: impl Into<String>,
        display_name: impl Into<String>,
        agent_type: impl Into<String>,
        state: AgentState,
        status_text: impl Into<String>,
        step_count: u32,
        metadata: SessionMetadata,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            display_name: display_name.into(),
            agent_type: agent_type.into(),
            state,
            status_text: status_text.into(),
            step_count,
            metadata,
            timestamp: Utc::now(),
        }
    }

    /// Formats a clean Winamp-style channel badge including tmux metadata if present
    pub fn format_channel_label(&self) -> String {
        if let Some(ref tmux_s) = self.metadata.tmux_session {
            if let Some(ref tmux_w) = self.metadata.tmux_window {
                format!("{}:{}", tmux_s, tmux_w)
            } else {
                tmux_s.clone()
            }
        } else {
            self.display_name.clone()
        }
    }
}
