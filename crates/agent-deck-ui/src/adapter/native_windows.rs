use super::StreamAdapter;
use agent_deck_core::{AgentState, SessionEvent, SessionMetadata};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, SystemTime};

pub struct NativeWindowsAdapter;

impl NativeWindowsAdapter {
    pub fn new() -> Self {
        Self
    }
}

fn extract_topic_from_transcript(path: &Path) -> Option<String> {
    if let Ok(file) = File::open(path) {
        let reader = BufReader::new(file);
        for line in reader.lines().flatten().take(10) {
            if let Ok(json) = serde_json::from_str::<Value>(&line) {
                let step_type = json.get("type").and_then(|v| v.as_str()).unwrap_or("");
                let source = json.get("source").and_then(|v| v.as_str()).unwrap_or("");

                if step_type == "USER_INPUT" || source == "USER_EXPLICIT" {
                    if let Some(content) = json.get("content").and_then(|v| v.as_str()) {
                        let text = if let Some(start) = content.find("<USER_REQUEST>") {
                            let after = &content[start + "<USER_REQUEST>".len()..];
                            if let Some(end) = after.find("</USER_REQUEST>") {
                                &after[..end]
                            } else {
                                after
                            }
                        } else {
                            content
                        };

                        let clean: String = text
                            .lines()
                            .map(|l| l.trim())
                            .filter(|l| !l.is_empty() && !l.starts_with('<'))
                            .collect::<Vec<&str>>()
                            .join(" ");

                        let trimmed = clean.trim();
                        if !trimmed.is_empty() {
                            let max_chars = 34;
                            let char_count = trimmed.chars().count();
                            let mut result: String = trimmed.chars().take(max_chars).collect();
                            if char_count > max_chars {
                                result.push_str("..");
                            }
                            return Some(result);
                        }
                    }
                }
            }
        }
    }
    None
}

impl StreamAdapter for NativeWindowsAdapter {
    fn name(&self) -> &'static str {
        "Native Windows Gemini Adapter"
    }

    fn start(&mut self, tx: Sender<SessionEvent>) {
        thread::spawn(move || {
            let home_dir = std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\Users\\schordinger".to_string());
            let brain_dir = PathBuf::from(home_dir).join(".gemini\\antigravity-cli\\brain");

            let mut watched_files: HashMap<PathBuf, u64> = HashMap::new();
            let mut session_topics: HashMap<PathBuf, String> = HashMap::new();

            loop {
                thread::sleep(Duration::from_millis(400));

                if !brain_dir.exists() {
                    continue;
                }

                if let Ok(entries) = std::fs::read_dir(&brain_dir) {
                    let mut candidate_sessions: Vec<(PathBuf, String, SystemTime)> = Vec::new();

                    for entry in entries.flatten() {
                        if let Ok(file_type) = entry.file_type() {
                            if file_type.is_dir() {
                                let session_id = entry.file_name().to_string_lossy().to_string();
                                let transcript_path = entry.path().join(".system_generated\\logs\\transcript.jsonl");
                                if transcript_path.exists() {
                                    if let Ok(meta) = std::fs::metadata(&transcript_path) {
                                        if let Ok(modified) = meta.modified() {
                                            // Consider active sessions from the last 48 hours
                                            if let Ok(elapsed) = SystemTime::now().duration_since(modified) {
                                                if elapsed < Duration::from_secs(48 * 3600) {
                                                    candidate_sessions.push((transcript_path, session_id, modified));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Sort candidates by most recently modified
                    candidate_sessions.sort_by(|a, b| b.2.cmp(&a.2));

                    // Process ALL active / recent sessions simultaneously
                    for (transcript_path, session_id, _) in candidate_sessions {
                        let last_pos = watched_files.get(&transcript_path).copied().unwrap_or(0);

                        // Resolve meaningful session topic/title
                        let topic = session_topics.entry(transcript_path.clone()).or_insert_with(|| {
                            extract_topic_from_transcript(&transcript_path)
                                .unwrap_or_else(|| format!("Session {}", &session_id[..6.min(session_id.len())]))
                        });

                        if let Ok(mut file) = File::open(&transcript_path) {
                            let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);

                            if file_len > last_pos || last_pos == 0 {
                                if last_pos == 0 && file_len > 8192 {
                                    let _ = file.seek(SeekFrom::Start(file_len - 8192));
                                } else {
                                    let _ = file.seek(SeekFrom::Start(last_pos));
                                }

                                let reader = BufReader::new(file);
                                let mut last_valid_line = None;
                                for line in reader.lines().flatten() {
                                    if !line.trim().is_empty() {
                                        last_valid_line = Some(line);
                                    }
                                }

                                watched_files.insert(transcript_path.clone(), file_len);

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
                                                    format!("TOOL {}: {} {}", tool_name, tool_summary, tool_action),
                                                )
                                            } else {
                                                (AgentState::Thinking, "THINKING • REASONING...".to_string())
                                            }
                                        } else if step_type == "USER_INPUT" || source == "USER_EXPLICIT" {
                                            let content = json.get("content").and_then(|v| v.as_str()).unwrap_or("");
                                            let clean_content = content
                                                .lines()
                                                .filter(|l| !l.starts_with('<') && !l.ends_with('>'))
                                                .collect::<Vec<&str>>()
                                                .join(" ");
                                            let preview: String = clean_content.chars().take(60).collect();
                                            (AgentState::Thinking, format!("PROCESSING: {}", preview))
                                        } else if step_type == "PLANNER_RESPONSE" && status == "DONE" {
                                            (
                                                AgentState::WaitingForInput {
                                                    prompt_preview: "Ready for input".to_string(),
                                                },
                                                "INPUT REQUIRED: Waiting for user prompt".to_string(),
                                            )
                                        } else {
                                            (AgentState::Thinking, format!("STEP #{}: {}", step_index, step_type))
                                        };

                                        let event = SessionEvent::new(
                                            format!("win-gemini-{}", session_id),
                                            topic.clone(),
                                            "Gemini",
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
