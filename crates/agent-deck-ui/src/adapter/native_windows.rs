use super::StreamAdapter;
use agent_deck_core::{AgentState, SessionEvent, SessionMetadata};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

pub struct NativeWindowsAdapter;

impl NativeWindowsAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl StreamAdapter for NativeWindowsAdapter {
    fn name(&self) -> &'static str {
        "Native Windows AGY Adapter"
    }

    fn start(&mut self, tx: Sender<SessionEvent>) {
        thread::spawn(move || {
            let home_dir = std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\Users\\schordinger".to_string());
            let brain_dir = PathBuf::from(home_dir).join(".gemini\\antigravity-cli\\brain");

            let mut current_watched_file: Option<PathBuf> = None;
            let mut last_file_pos: u64 = 0;

            loop {
                thread::sleep(Duration::from_millis(450));

                // 1. Find the latest modified session folder
                if let Ok(entries) = std::fs::read_dir(&brain_dir) {
                    let mut latest_dir: Option<(PathBuf, String, std::time::SystemTime)> = None;
                    for entry in entries.flatten() {
                        if let Ok(file_type) = entry.file_type() {
                            if file_type.is_dir() {
                                let session_id = entry.file_name().to_string_lossy().to_string();
                                let transcript_path = entry.path().join(".system_generated\\logs\\transcript.jsonl");
                                if transcript_path.exists() {
                                    if let Ok(meta) = std::fs::metadata(&transcript_path) {
                                        if let Ok(modified) = meta.modified() {
                                            if latest_dir.as_ref().map_or(true, |(_, _, latest_time)| modified > *latest_time) {
                                                latest_dir = Some((transcript_path, session_id, modified));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if let Some((latest_transcript, session_id, _)) = latest_dir {
                        if current_watched_file.as_ref() != Some(&latest_transcript) {
                            current_watched_file = Some(latest_transcript.clone());
                            last_file_pos = 0;
                        }

                        if let Ok(mut file) = File::open(&latest_transcript) {
                            let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);

                            if file_len > last_file_pos {
                                if last_file_pos == 0 && file_len > 8192 {
                                    let _ = file.seek(SeekFrom::Start(file_len - 8192));
                                } else {
                                    let _ = file.seek(SeekFrom::Start(last_file_pos));
                                }

                                let reader = BufReader::new(file);
                                let mut last_valid_line = None;
                                for line in reader.lines().flatten() {
                                    if !line.trim().is_empty() {
                                        last_valid_line = Some(line);
                                    }
                                }

                                last_file_pos = file_len;

                                if let Some(line) = last_valid_line {
                                    if let Ok(json) = serde_json::from_str::<Value>(&line) {
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
                                                    prompt_preview: "Ready for input".to_string(),
                                                },
                                                "WAITING FOR USER INPUT / PROMPT".to_string(),
                                            )
                                        } else {
                                            (AgentState::Thinking, format!("STEP #{}: {}", step_index, step_type))
                                        };

                                        let event = SessionEvent::new(
                                            format!("win-agy-{}", session_id),
                                            "AGY (Win Native)",
                                            "AGY-WIN",
                                            state,
                                            status_text,
                                            step_index,
                                            SessionMetadata {
                                                host: "Windows".to_string(),
                                                tmux_session: None,
                                                tmux_window: None,
                                                tmux_pane: None,
                                                cwd: None,
                                                pid: None,
                                            },
                                        );

                                        let _ = tx.send(event);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
    }
}

