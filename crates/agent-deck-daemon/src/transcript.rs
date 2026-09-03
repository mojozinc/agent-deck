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
    session_titles: HashMap<PathBuf, String>,
    latest_sessions: HashMap<String, SessionEvent>, // SessionId -> Latest Event
    brain_dir: PathBuf,
    distro_name: String,
}

fn extract_earliest_markdown_heading(brain_session_dir: &Path) -> Option<String> {
    if let Ok(entries) = std::fs::read_dir(brain_session_dir) {
        let mut md_files: Vec<(PathBuf, SystemTime)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().map_or(false, |ext| ext.eq_ignore_ascii_case("md")) {
                if let Ok(meta) = path.metadata() {
                    let created = meta.created().or_else(|_| meta.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
                    md_files.push((path, created));
                }
            }
        }

        md_files.sort_by(|a, b| a.1.cmp(&b.1));

        for (md_path, _) in md_files {
            if let Ok(file) = File::open(&md_path) {
                let reader = BufReader::new(file);
                for line in reader.lines().flatten().take(30) {
                    let trimmed = line.trim();
                    if trimmed.starts_with("# ") {
                        let header_content = trimmed.trim_start_matches("# ").trim();
                        let clean = header_content
                            .trim_start_matches("Implementation Plan:")
                            .trim_start_matches("Plan:")
                            .trim_start_matches("Walkthrough:")
                            .replace(['*', '_', '`'], "")
                            .trim()
                            .to_string();

                        if !clean.is_empty() {
                            let max_chars = 34;
                            let char_count = clean.chars().count();
                            let mut result: String = clean.chars().take(max_chars).collect();
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

fn extract_workdir_basename(transcript_path: &Path) -> Option<String> {
    if let Ok(file) = File::open(transcript_path) {
        let reader = BufReader::new(file);
        for line in reader.lines().flatten().take(12) {
            if let Ok(json) = serde_json::from_str::<Value>(&line) {
                if let Some(content) = json.get("content").and_then(|v| v.as_str()) {
                    if let Some(pos) = content.find("workbench") {
                        let slice = &content[pos..pos + 80.min(content.len() - pos)];
                        let path_part = slice.lines().next().unwrap_or("").trim();
                        let clean_path = path_part.split("->").next().unwrap_or(path_part).trim();
                        let path_obj = Path::new(clean_path);
                        if let Some(name) = path_obj.file_name().and_then(|n| n.to_str()) {
                            let name_clean = name.trim().trim_matches(['\\', '/', ' ', '\r', '\n']);
                            if !name_clean.is_empty() && name_clean != "workbench" {
                                return Some(name_clean.to_string());
                            }
                        }
                    }
                }

                if let Some(tool_calls) = json.get("tool_calls").and_then(|v| v.as_array()) {
                    for tool in tool_calls {
                        if let Some(args) = tool.get("args") {
                            if let Some(cwd) = args.get("Cwd").and_then(|v| v.as_str()) {
                                if let Some(name) = Path::new(cwd).file_name().and_then(|n| n.to_str()) {
                                    if !name.trim().is_empty() {
                                        return Some(name.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

impl TranscriptWatcher {
    pub fn new() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        let brain_dir = PathBuf::from(home).join(".gemini/antigravity-cli/brain");
        let distro_name = std::env::var("WSL_DISTRO_NAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "clibox".to_string());

        Self {
            watched_sessions: HashMap::new(),
            session_titles: HashMap::new(),
            latest_sessions: HashMap::new(),
            brain_dir,
            distro_name,
        }
    }

    /// Returns a snapshot of all currently known active sessions
    pub fn get_latest_sessions(&self) -> Vec<SessionEvent> {
        self.latest_sessions.values().cloned().collect()
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
                    // Only process active/recent sessions modified within the last 3 days
                    if let Ok(meta) = std::fs::metadata(&transcript_file) {
                        if let Ok(modified) = meta.modified() {
                            if let Ok(elapsed) = modified.elapsed() {
                                if elapsed.as_secs() > 86400 * 3 {
                                    continue;
                                }
                            }
                        }
                    }

                    if let Some(event) = self.check_transcript_file(&session_id, &session_path, &transcript_file) {
                        events.push(event);
                    }
                }
            }
        }

        events
    }

    fn check_transcript_file(&mut self, session_id: &str, session_dir: &Path, path: &Path) -> Option<SessionEvent> {
        let file_meta = std::fs::metadata(path).ok()?;
        let file_len = file_meta.len();
        let last_pos = self.watched_sessions.get(path).copied().unwrap_or(0);

        if file_len <= last_pos && last_pos > 0 {
            return None;
        }

        let mut file = File::open(path).ok()?;

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

                // In CLI mode, any proposed tool step awaiting output requires user permission/confirmation
                (
                    AgentState::WaitingForApproval {
                        name: tool_name.to_string(),
                        summary: tool_summary.to_string(),
                    },
                    if tool_action.is_empty() {
                        format!("PERMISSION REQUIRED: {} - {}", tool_name, tool_summary)
                    } else {
                        format!("PERMISSION REQUIRED: {} ({})", tool_summary, tool_action)
                    },
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
                "WAITING FOR PROMPT".to_string(),
            )
        } else {
            (AgentState::Thinking, format!("STEP #{}: {}", step_index, step_type))
        };

        // Query tmux metadata
        let tmux_info = TmuxInspector::resolve_metadata(None, None);
        let metadata = SessionMetadata {
            host: format!("wsl:{}", self.distro_name),
            tmux_session: tmux_info.as_ref().map(|(s, _, _)| s.clone()),
            tmux_window: tmux_info.as_ref().map(|(_, w, _)| w.clone()),
            tmux_pane: tmux_info.as_ref().map(|(_, _, p)| p.clone()),
            cwd: None,
            pid: None,
        };

        let current_title = self.session_titles.get(path).cloned();
        let is_placeholder = current_title.as_ref().map_or(true, |t| t.starts_with("Session "));

        let title = if is_placeholder {
            let upgraded = if let Some(heading) = extract_earliest_markdown_heading(session_dir) {
                heading
            } else if let Some(workdir) = extract_workdir_basename(path) {
                workdir
            } else {
                format!("Session {}", &session_id[..6.min(session_id.len())])
            };
            self.session_titles.insert(path.to_path_buf(), upgraded.clone());
            upgraded
        } else {
            current_title.unwrap()
        };

        let event = SessionEvent::new(
            format!("wsl-{}-{}", self.distro_name, session_id),
            title,
            "Gemini",
            state,
            status_text,
            step_index,
            metadata,
        );

        self.latest_sessions.insert(session_id.to_string(), event.clone());
        Some(event)
    }
}
