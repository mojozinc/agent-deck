use crate::tmux::TmuxInspector;
use agent_deck_core::{AgentState, SessionEvent, SessionMetadata};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub struct TranscriptWatcher {
    watched_sessions: HashMap<PathBuf, u64>, // Path -> Last read byte position
    brain_dir: PathBuf,
}

impl TranscriptWatcher {
    pub fn new() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        let brain_dir = PathBuf::from(home).join(".gemini/antigravity-cli/brain");
        Self {
            watched_sessions: HashMap::new(),
            brain_dir,
        }
    }

    /// Scans the brain directory for any active transcripts with new events
    pub fn scan_and_collect_events(&mut self) -> Vec<SessionEvent> {
        let mut events = Vec::new();

        if !self.brain_dir.exists() {
            return events;
        }

        let entries = match std::fs::read_dir(&self.brain_dir) {
            Ok(e) => e,
            Err(_) => return events,
        };

        for entry in entries.flatten() {
            let session_path = entry.path();
            if session_path.is_dir() {
                let session_id = session_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string();

                let transcript_file = session_path.join(".system_generated/logs/transcript.jsonl");
                if transcript_file.exists() {
                    if let Some(event) = self.check_transcript_file(&session_id, &transcript_file) {
                        events.push(event);
                    }
                }
            }
        }

        events
    }

    fn check_transcript_file(&mut self, session_id: &str, path: &Path) -> Option<SessionEvent> {
        let file_meta = std::fs::metadata(path).ok()?;
        let file_len = file_meta.len();
        let last_pos = self.watched_sessions.get(path).copied().unwrap_or(0);

        if file_len <= last_pos && last_pos > 0 {
            return None;
        }

        let mut file = File::open(path).ok()?;

        // If newly discovered file, seek to tail
        if last_pos == 0 && file_len > 8192 {
            let _ = file.seek(SeekFrom::Start(file_len - 8192));
        } else {
            let _ = file.seek(SeekFrom::Start(last_pos));
        }

        let reader = BufReader::new(file);
        let mut last_line = None;

        for line in reader.lines().flatten() {
            if !line.trim().is_empty() {
                last_line = Some(line);
            }
        }

        self.watched_sessions.insert(path.to_path_buf(), file_len);

        let line = last_line?;
        let json = serde_json::from_str::<Value>(&line).ok()?;

        let step_index = json.get("step_index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let step_type = json.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let source = json.get("source").and_then(|v| v.as_str()).unwrap_or("");
        let status = json.get("status").and_then(|v| v.as_str()).unwrap_or("");

        let (state, status_text) = if let Some(tool_calls) = json.get("tool_calls").and_then(|v| v.as_array()) {
            if let Some(first_tool) = tool_calls.first() {
                let tool_name = first_tool.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                let tool_summary = first_tool
                    .get("args")
                    .and_then(|a| a.get("toolSummary"))
                    .and_then(|s| s.as_str())
                    .unwrap_or(tool_name);
                let tool_action = first_tool
                    .get("args")
                    .and_then(|a| a.get("toolAction"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("");

                (
                    AgentState::RunningTool {
                        name: tool_name.to_string(),
                        summary: tool_summary.to_string(),
                    },
                    format!("TOOL [{}]: {} - {}", tool_name, tool_summary, tool_action),
                )
            } else {
                (AgentState::Thinking, "THINKING / REASONING...".to_string())
            }
        } else if step_type == "USER_INPUT" || source == "USER_EXPLICIT" {
            let content = json.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let preview: String = content.chars().take(60).collect();
            (AgentState::Thinking, format!("PROCESSING PROMPT: {}", preview))
        } else if step_type == "PLANNER_RESPONSE" && status == "DONE" {
            (
                AgentState::WaitingForInput {
                    prompt_preview: "Ready for user input".to_string(),
                },
                "WAITING FOR USER INPUT / PROMPT".to_string(),
            )
        } else {
            (AgentState::Thinking, format!("STEP #{}: {}", step_index, step_type))
        };

        // Query tmux metadata
        let tmux_info = TmuxInspector::resolve_metadata(None, None);
        let metadata = SessionMetadata {
            host: "WSL2-Ubuntu".to_string(),
            tmux_session: tmux_info.as_ref().map(|(s, _, _)| s.clone()),
            tmux_window: tmux_info.as_ref().map(|(_, w, _)| w.clone()),
            tmux_pane: tmux_info.as_ref().map(|(_, _, p)| p.clone()),
            cwd: None,
            pid: None,
        };

        let display_name = if let Some(ref s) = metadata.tmux_session {
            format!("AGY [tmux:{}]", s)
        } else {
            "AGY [WSL2]".to_string()
        };

        Some(SessionEvent::new(
            format!("wsl-agy-{}", session_id),
            display_name,
            "AGY-WSL",
            state,
            status_text,
            step_index,
            metadata,
        ))
    }
}

