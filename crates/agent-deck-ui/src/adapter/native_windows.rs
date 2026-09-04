use super::StreamAdapter;
use agent_deck_core::{
    extract_claude_title, extract_earliest_markdown_heading, extract_prompt_fallback,
    extract_workdir_basename, AgentState, AntigravityParser, ClaudeParser, SafeLineReader,
    SessionEvent, SessionMetadata,
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader};
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

/// Checks if an OS file lock is actively held by the root CLI process
fn is_session_process_active(presence_dir: &Path, session_id: &str) -> bool {
    let lock_file = presence_dir.join(format!("{}.lock", session_id));
    if !lock_file.exists() {
        return false;
    }

    #[cfg(target_os = "windows")]
    {
        match OpenOptions::new().read(true).write(true).share_mode(0).open(&lock_file) {
            Ok(_) => false, // Successfully opened -> NOT locked -> Dead root session
            Err(_) => true, // Sharing violation -> ACTIVELY LOCKED by agy.exe
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        true
    }
}

/// Recursively traverses the subagent tree to find all descendant subagents spawned under this root session
fn extract_all_descendant_subagents(brain_dir: &Path, root_session_id: &str) -> HashSet<String> {
    let mut all_descendants = HashSet::new();
    let mut queue = vec![root_session_id.to_string()];
    let mut visited = HashSet::new();
    visited.insert(root_session_id.to_string());

    while let Some(current_id) = queue.pop() {
        let transcript_path = brain_dir
            .join(&current_id)
            .join(".system_generated\\logs\\transcript.jsonl");

        if let Ok(file) = File::open(&transcript_path) {
            let reader = BufReader::new(file);
            for line in reader.lines().flatten() {
                if line.contains("conversationId") || line.contains("invoke_subagent") {
                    if let Ok(json) = serde_json::from_str::<Value>(&line) {
                        if let Some(content) = json.get("content").and_then(|v| v.as_str()) {
                            let mut start = 0;
                            while let Some(pos) = content[start..].find("conversationId") {
                                let idx = start + pos;
                                let slice = &content[idx..idx + 80.min(content.len() - idx)];
                                if let Some(first_quote) = slice.find(':') {
                                    let after_colon = &slice[first_quote + 1..];
                                    if let Some(quote_start) = after_colon.find('"') {
                                        let rest = &after_colon[quote_start + 1..];
                                        if let Some(quote_end) = rest.find('"') {
                                            let candidate_id = rest[..quote_end].trim();
                                            if candidate_id.len() == 36 && candidate_id.contains('-') {
                                                if visited.insert(candidate_id.to_string()) {
                                                    all_descendants.insert(candidate_id.to_string());
                                                    queue.push(candidate_id.to_string());
                                                }
                                            }
                                        }
                                    }
                                }
                                start = idx + 14;
                            }
                        }
                    }
                }
            }
        }
    }

    all_descendants
}

/// Checks for active background subagents explicitly belonging to this parent session tree
fn find_active_subagent_activity(brain_dir: &Path, spawned_ids: &HashSet<String>) -> Option<(usize, String)> {
    if spawned_ids.is_empty() {
        return None;
    }

    let mut active_count = 0;
    let mut latest_activity = String::new();
    let mut latest_mod = SystemTime::UNIX_EPOCH;

    for subagent_id in spawned_ids {
        let subagent_dir = brain_dir.join(subagent_id);
        let transcript = subagent_dir.join(".system_generated\\logs\\transcript.jsonl");

        if let Ok(meta) = std::fs::metadata(&transcript) {
            let file_len = meta.len();
            if let Ok(modified) = meta.modified() {
                if let Ok(elapsed) = SystemTime::now().duration_since(modified) {
                    if elapsed < Duration::from_secs(150) {
                        active_count += 1;
                        if modified > latest_mod {
                            latest_mod = modified;
                            if let Ok(mut file) = File::open(&transcript) {
                                let mut sub_activity = String::new();
                                let _ = SafeLineReader::read_new_lines(
                                    &mut file,
                                    file_len,
                                    0,
                                    8192,
                                    |_line, json| {
                                        if let Some(step) = AntigravityParser::parse_step(json) {
                                            sub_activity = step.status_text;
                                        }
                                        Ok(())
                                    },
                                );
                                if !sub_activity.is_empty() {
                                    latest_activity = sub_activity;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if active_count > 0 {
        if latest_activity.is_empty() {
            latest_activity = "Exploring codebase & executing tasks".to_string();
        }
        Some((active_count, latest_activity))
    } else {
        None
    }
}

impl StreamAdapter for NativeWindowsAdapter {
    fn name(&self) -> &'static str {
        "Native Windows Gemini & Claude Adapter"
    }

    fn start(&mut self, tx: Sender<SessionEvent>) {
        thread::spawn(move || {
            let home_dir = std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\Users\\schordinger".to_string());
            let brain_dir = PathBuf::from(&home_dir).join(".gemini\\antigravity-cli\\brain");
            let presence_dir = PathBuf::from(&home_dir).join(".gemini\\antigravity-cli\\presence");
            let claude_dir = PathBuf::from(&home_dir).join(".claude");

            let mut watched_files: HashMap<PathBuf, u64> = HashMap::new();
            let mut session_titles: HashMap<PathBuf, String> = HashMap::new();
            let mut active_agy_sessions: HashMap<String, PathBuf> = HashMap::new();

            loop {
                thread::sleep(Duration::from_millis(300));

                // 1. Ingest Antigravity CLI Sessions
                if brain_dir.exists() {
                    if let Ok(entries) = std::fs::read_dir(&brain_dir) {
                        let mut candidate_sessions: Vec<(PathBuf, PathBuf, String, SystemTime)> = Vec::new();
                        let mut currently_active: HashMap<String, PathBuf> = HashMap::new();

                        for entry in entries.flatten() {
                            if let Ok(file_type) = entry.file_type() {
                                if file_type.is_dir() {
                                    let session_dir = entry.path();
                                    let session_id = entry.file_name().to_string_lossy().to_string();

                                    // Only show root interactive CLI sessions (presence lock must exist and be locked)
                                    if !is_session_process_active(&presence_dir, &session_id) {
                                        continue;
                                    }

                                    let transcript_path = session_dir.join(".system_generated\\logs\\transcript.jsonl");
                                    if transcript_path.exists() {
                                        currently_active.insert(session_id.clone(), transcript_path.clone());
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

                        // Clean up exited sessions: detect sessions that were active but are no longer active
                        for (exited_id, path) in &active_agy_sessions {
                            if !currently_active.contains_key(exited_id) {
                                watched_files.remove(path);
                                session_titles.remove(path);
                                let exit_event = SessionEvent::new(
                                    format!("win-gemini-{}", exited_id),
                                    "Session Exited",
                                    "Gemini",
                                    AgentState::Exited,
                                    "Session terminated",
                                    0,
                                    SessionMetadata {
                                        host: "Windows".to_string(),
                                        tmux_session: None,
                                        tmux_window: None,
                                        tmux_pane: None,
                                        cwd: None,
                                        pid: None,
                                        agent_type: Some("Gemini".to_string()),
                                    },
                                );
                                let _ = tx.send(exit_event);
                            }
                        }
                        active_agy_sessions = currently_active;

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
                                    let mut latest_step = None;

                                    if let Ok((new_pos, count)) = SafeLineReader::read_new_lines(
                                        &mut file,
                                        file_len,
                                        last_pos,
                                        8192,
                                        |_line_str, json_val| {
                                            if let Some(step) = AntigravityParser::parse_step(json_val) {
                                                latest_step = Some(step);
                                            }
                                            Ok(())
                                        },
                                    ) {
                                        watched_files.insert(transcript_path.clone(), new_pos);

                                        if count > 0 || latest_step.is_some() {
                                            if let Some(step) = latest_step {
                                                let mut state = step.state;
                                                let mut status_text = step.status_text;

                                                // Check if this parent session has actively running spawned subagents
                                                let descendant_subagents = extract_all_descendant_subagents(&brain_dir, &session_id);
                                                if let Some((sub_count, sub_activity)) =
                                                    find_active_subagent_activity(&brain_dir, &descendant_subagents)
                                                {
                                                    state = AgentState::RunningTool {
                                                        name: format!("{}-subagents", sub_count),
                                                        summary: sub_activity.clone(),
                                                    };
                                                    status_text = sub_activity;
                                                }

                                                let event = SessionEvent::new(
                                                    format!("win-gemini-{}", session_id),
                                                    title.clone(),
                                                    "Gemini",
                                                    state,
                                                    status_text,
                                                    step.step_index,
                                                    SessionMetadata {
                                                        host: "Windows".to_string(),
                                                        tmux_session: None,
                                                        tmux_window: None,
                                                        tmux_pane: None,
                                                        cwd: None,
                                                        pid: None,
                                                        agent_type: Some("Gemini".to_string()),
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
                }

                // 2. Ingest Claude Code Sessions
                if claude_dir.exists() {
                    let projects_dir = claude_dir.join("projects");
                    if let Ok(project_entries) = std::fs::read_dir(&projects_dir) {
                        for project_entry in project_entries.flatten() {
                            let project_path = project_entry.path();
                            if project_path.is_dir() {
                                if let Ok(files) = std::fs::read_dir(&project_path) {
                                    for file_entry in files.flatten() {
                                        let transcript_path = file_entry.path();
                                        if transcript_path.is_file()
                                            && transcript_path.extension().map_or(false, |ext| ext == "jsonl")
                                        {
                                            if let Ok(meta) = std::fs::metadata(&transcript_path) {
                                                if let Ok(modified) = meta.modified() {
                                                    if let Ok(elapsed) = SystemTime::now().duration_since(modified) {
                                                        if elapsed > Duration::from_secs(48 * 3600) {
                                                            continue;
                                                        }
                                                    }
                                                }
                                            }

                                            let last_pos = watched_files.get(&transcript_path).copied().unwrap_or(0);

                                            if let Ok(mut file) = File::open(&transcript_path) {
                                                let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);

                                                if file_len > last_pos || last_pos == 0 {
                                                    let mut latest_step = None;
                                                    let mut step_count = 0u32;

                                                    if let Ok((new_pos, count)) = SafeLineReader::read_new_lines(
                                                        &mut file,
                                                        file_len,
                                                        last_pos,
                                                        8192,
                                                        |_line_str, json_val| {
                                                            step_count += 1;
                                                            if let Some(step) = ClaudeParser::parse_line(json_val, step_count) {
                                                                latest_step = Some(step);
                                                            }
                                                            Ok(())
                                                        },
                                                    ) {
                                                        watched_files.insert(transcript_path.clone(), new_pos);

                                                        if count > 0 || latest_step.is_some() {
                                                            if let Some(step) = latest_step {
                                                                let raw_id = step.session_id.clone().unwrap_or_else(|| {
                                                                    transcript_path
                                                                        .file_stem()
                                                                        .and_then(|s| s.to_str())
                                                                        .unwrap_or("unknown")
                                                                        .to_string()
                                                                });

                                                                let current_title = session_titles.get(&transcript_path).cloned();
                                                                let is_placeholder = current_title.as_ref().map_or(true, |t| {
                                                                    t.starts_with("Claude ") || t == "Claude Session"
                                                                });

                                                                let title = if is_placeholder {
                                                                    let upgraded = extract_claude_title(&transcript_path, &project_path);
                                                                    session_titles.insert(transcript_path.clone(), upgraded.clone());
                                                                    upgraded
                                                                } else {
                                                                    current_title.unwrap()
                                                                };

                                                                let event = SessionEvent::new(
                                                                    format!("win-claude-{}", raw_id),
                                                                    title,
                                                                    "Claude",
                                                                    step.state,
                                                                    step.status_text,
                                                                    step.step_index,
                                                                    SessionMetadata {
                                                                        host: "Windows".to_string(),
                                                                        tmux_session: None,
                                                                        tmux_window: None,
                                                                        tmux_pane: None,
                                                                        cwd: step.cwd,
                                                                        pid: None,
                                                                        agent_type: Some("Claude".to_string()),
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
                                }
                            }
                        }
                    }

                    // Also check sessions/ directory under .claude if present
                    let sessions_dir = claude_dir.join("sessions");
                    if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
                        for entry in entries.flatten() {
                            let transcript_path = entry.path();
                            if transcript_path.is_file()
                                && transcript_path.extension().map_or(false, |ext| ext == "jsonl")
                            {
                                if let Ok(meta) = std::fs::metadata(&transcript_path) {
                                    if let Ok(modified) = meta.modified() {
                                        if let Ok(elapsed) = SystemTime::now().duration_since(modified) {
                                            if elapsed > Duration::from_secs(48 * 3600) {
                                                continue;
                                            }
                                        }
                                    }
                                }

                                let last_pos = watched_files.get(&transcript_path).copied().unwrap_or(0);

                                if let Ok(mut file) = File::open(&transcript_path) {
                                    let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);

                                    if file_len > last_pos || last_pos == 0 {
                                        let mut latest_step = None;
                                        let mut step_count = 0u32;

                                        if let Ok((new_pos, count)) = SafeLineReader::read_new_lines(
                                            &mut file,
                                            file_len,
                                            last_pos,
                                            8192,
                                            |_line_str, json_val| {
                                                step_count += 1;
                                                if let Some(step) = ClaudeParser::parse_line(json_val, step_count) {
                                                    latest_step = Some(step);
                                                }
                                                Ok(())
                                            },
                                        ) {
                                            watched_files.insert(transcript_path.clone(), new_pos);

                                            if count > 0 || latest_step.is_some() {
                                                if let Some(step) = latest_step {
                                                    let raw_id = step.session_id.clone().unwrap_or_else(|| {
                                                        transcript_path
                                                            .file_stem()
                                                            .and_then(|s| s.to_str())
                                                            .unwrap_or("unknown")
                                                            .to_string()
                                                    });

                                                    let title = extract_claude_title(&transcript_path, &sessions_dir);

                                                    let event = SessionEvent::new(
                                                        format!("win-claude-{}", raw_id),
                                                        title,
                                                        "Claude",
                                                        step.state,
                                                        step.status_text,
                                                        step.step_index,
                                                        SessionMetadata {
                                                            host: "Windows".to_string(),
                                                            tmux_session: None,
                                                            tmux_window: None,
                                                            tmux_pane: None,
                                                            cwd: step.cwd,
                                                            pid: None,
                                                            agent_type: Some("Claude".to_string()),
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
                    }
                }
            }
        });
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_native_windows_adapter_instantiation() {
        let adapter = NativeWindowsAdapter::new();
        assert_eq!(adapter.name(), "Native Windows Gemini & Claude Adapter");
    }

    #[test]
    fn test_extract_all_descendant_subagents() {
        let temp_dir = std::env::temp_dir().join(format!(
            "test_descendant_subagents_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root_id = "00000000-1111-2222-3333-444444444444";
        let sub1_id = "11111111-2222-3333-4444-555555555555";
        let sub2_id = "22222222-3333-4444-5555-666666666666";

        let root_logs = temp_dir.join(root_id).join(".system_generated\\logs");
        let sub1_logs = temp_dir.join(sub1_id).join(".system_generated\\logs");
        std::fs::create_dir_all(&root_logs).unwrap();
        std::fs::create_dir_all(&sub1_logs).unwrap();

        {
            let mut f = File::create(root_logs.join("transcript.jsonl")).unwrap();
            writeln!(
                f,
                "{{\"content\": \"spawn child with conversationId: \\\"{}\\\"\"}}",
                sub1_id
            )
            .unwrap();
        }

        {
            let mut f = File::create(sub1_logs.join("transcript.jsonl")).unwrap();
            writeln!(
                f,
                "{{\"content\": \"spawn grand-child with conversationId: \\\"{}\\\"\"}}",
                sub2_id
            )
            .unwrap();
        }

        let descendants = extract_all_descendant_subagents(&temp_dir, root_id);
        assert_eq!(descendants.len(), 2);
        assert!(descendants.contains(sub1_id));
        assert!(descendants.contains(sub2_id));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_subagent_activity_detection_when_empty() {
        let empty_set = HashSet::new();
        let dummy_path = Path::new("C:\\dummy");
        assert_eq!(find_active_subagent_activity(dummy_path, &empty_set), None);
    }

    #[test]
    fn test_subagent_activity_detection_when_active() {
        let temp_dir = std::env::temp_dir().join(format!(
            "test_subagents_brain_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let sub_id = "11111111-2222-3333-4444-555555555555";
        let sub_dir = temp_dir.join(sub_id);
        let logs_dir = sub_dir.join(".system_generated\\logs");
        std::fs::create_dir_all(&logs_dir).unwrap();

        let transcript = logs_dir.join("transcript.jsonl");
        {
            let mut f = File::create(&transcript).unwrap();
            writeln!(
                f,
                "{{\"step_index\": 1, \"type\": \"PLANNER_RESPONSE\", \"status\": \"RUNNING\", \"tool_calls\": [{{\"name\": \"run_command\", \"args\": {{\"toolSummary\": \"Running test command\"}}}}]}}"
            )
            .unwrap();
        }

        let mut spawned = HashSet::new();
        spawned.insert(sub_id.to_string());

        let activity = find_active_subagent_activity(&temp_dir, &spawned);
        assert!(activity.is_some());
        let (count, summary) = activity.unwrap();
        assert_eq!(count, 1);
        assert!(summary.contains("RUNNING TOOL"));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_is_session_process_active_dead_and_missing() {
        let temp_dir = std::env::temp_dir().join(format!(
            "test_presence_liveness_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let dummy_id = "test-session-liveness-uuid";

        // 1. Lock file does not exist -> returns false (dead/exited)
        assert!(!is_session_process_active(&temp_dir, dummy_id));

        // 2. Lock file exists on disk, but NO process holds an exclusive lock on it -> returns false (dead/exited)
        let lock_file = temp_dir.join(format!("{}.lock", dummy_id));
        File::create(&lock_file).unwrap();
        assert!(!is_session_process_active(&temp_dir, dummy_id));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
