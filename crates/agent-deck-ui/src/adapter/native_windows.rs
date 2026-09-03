use super::StreamAdapter;
use agent_deck_core::{AgentState, SessionEvent, SessionMetadata};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, SystemTime};

#[cfg(target_os = "windows")]
use std::os::windows::fs::OpenOptionsExt;

pub struct NativeWindowsAdapter;

impl NativeWindowsAdapter {
    pub fn new() -> Self {
        Self
    }
}

/// Checks if an OS file lock is actively held by the CLI process
fn is_session_process_active(presence_dir: &Path, session_id: &str) -> bool {
    let lock_file = presence_dir.join(format!("{}.lock", session_id));
    if !lock_file.exists() {
        return false;
    }

    #[cfg(target_os = "windows")]
    {
        // Attempt to open exclusively with share_mode(0) (zero sharing allowed).
        // If agy.exe holds the file open, Windows kernel returns a Sharing Violation (Err) -> ACTIVELY RUNNING.
        // If agy.exe has exited, OpenOptions succeeds (Ok) -> DEAD / TERMINATED.
        match OpenOptions::new().read(true).write(true).share_mode(0).open(&lock_file) {
            Ok(_) => false, // Successfully opened exclusively -> No process holds it -> Dead
            Err(_) => true, // Sharing violation -> ACTIVELY LOCKED by agy.exe -> Alive
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        true
    }
}

/// Priority 1: Heading #1 in earliest markdown file in the session's brain directory
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

/// Priority 2: Workdir basename extracted from transcript
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
                            if let Some(search_path) = args.get("SearchPath").and_then(|v| v.as_str()) {
                                if let Some(name) = Path::new(search_path).file_name().and_then(|n| n.to_str()) {
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

/// Fallback: Initial user prompt
fn extract_prompt_fallback(transcript_path: &Path) -> Option<String> {
    if let Ok(file) = File::open(transcript_path) {
        let reader = BufReader::new(file);
        for line in reader.lines().flatten().take(8) {
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
                            let max_chars = 32;
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
            let brain_dir = PathBuf::from(&home_dir).join(".gemini\\antigravity-cli\\brain");
            let presence_dir = PathBuf::from(&home_dir).join(".gemini\\antigravity-cli\\presence");

            let mut watched_files: HashMap<PathBuf, u64> = HashMap::new();
            let mut session_titles: HashMap<PathBuf, String> = HashMap::new();

            loop {
                thread::sleep(Duration::from_millis(400));

                if !brain_dir.exists() {
                    continue;
                }

                if let Ok(entries) = std::fs::read_dir(&brain_dir) {
                    let mut candidate_sessions: Vec<(PathBuf, PathBuf, String, SystemTime)> = Vec::new();

                    for entry in entries.flatten() {
                        if let Ok(file_type) = entry.file_type() {
                            if file_type.is_dir() {
                                let session_dir = entry.path();
                                let session_id = entry.file_name().to_string_lossy().to_string();

                                // Liveness verification: Only process sessions whose OS presence lock is actively held!
                                if !is_session_process_active(&presence_dir, &session_id) {
                                    continue;
                                }

                                let transcript_path = session_dir.join(".system_generated\\logs\\transcript.jsonl");
                                if transcript_path.exists() {
                                    if let Ok(meta) = std::fs::metadata(&transcript_path) {
                                        if let Ok(modified) = meta.modified() {
                                            if let Ok(elapsed) = SystemTime::now().duration_since(modified) {
                                                if elapsed < Duration::from_secs(48 * 3600) {
                                                    candidate_sessions.push((session_dir, transcript_path, session_id, modified));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    candidate_sessions.sort_by(|a, b| b.3.cmp(&a.3));

                    for (session_dir, transcript_path, session_id, _) in candidate_sessions {
                        let last_pos = watched_files.get(&transcript_path).copied().unwrap_or(0);

                        // Resolve or dynamically upgrade title if previously a placeholder
                        let current_title = session_titles.get(&transcript_path).cloned();
                        let is_placeholder = current_title.as_ref().map_or(true, |t| t.starts_with("Session "));

                        let title = if is_placeholder {
                            let upgraded = if let Some(heading) = extract_earliest_markdown_heading(&session_dir) {
                                heading
                            } else if let Some(workdir) = extract_workdir_basename(&transcript_path) {
                                workdir
                            } else if let Some(prompt) = extract_prompt_fallback(&transcript_path) {
                                prompt
                            } else {
                                format!("Session {}", &session_id[..6.min(session_id.len())])
                            };
                            session_titles.insert(transcript_path.clone(), upgraded.clone());
                            upgraded
                        } else {
                            current_title.unwrap()
                        };

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

                                        let event = SessionEvent::new(
                                            format!("win-gemini-{}", session_id),
                                            title.clone(),
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
