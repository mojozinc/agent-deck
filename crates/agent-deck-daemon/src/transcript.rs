use crate::tmux::TmuxInspector;
use agent_deck_core::{
    extract_claude_title, extract_earliest_markdown_heading, extract_prompt_fallback,
    extract_workdir_basename, AgentState, AntigravityParser, ClaudeParser, SafeLineReader,
    SessionEvent, SessionMetadata,
};
use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};

#[cfg(unix)]
fn is_session_process_active(presence_dir: &Path, session_id: &str) -> bool {
    use std::os::unix::io::AsRawFd;

    let lock_file = presence_dir.join(format!("{}.lock", session_id));
    if !lock_file.exists() {
        return false;
    }

    if let Ok(file) = std::fs::File::open(&lock_file) {
        let fd = file.as_raw_fd();
        let res = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if res == 0 {
            unsafe { libc::flock(fd, libc::LOCK_UN) };
            false
        } else {
            true
        }
    } else {
        false
    }
}

#[cfg(not(unix))]
fn is_session_process_active(presence_dir: &Path, session_id: &str) -> bool {
    let lock_file = presence_dir.join(format!("{}.lock", session_id));
    if !lock_file.exists() {
        return false;
    }

    #[cfg(target_os = "windows")]
    {
        use std::fs::OpenOptions;
        use std::os::windows::fs::OpenOptionsExt;
        match OpenOptions::new().read(true).write(true).share_mode(0).open(&lock_file) {
            Ok(_) => false, // Opened -> Dead
            Err(_) => true, // Sharing violation -> Alive
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        true
    }
}

pub struct TranscriptWatcher {
    watched_sessions: HashMap<PathBuf, u64>, // Path -> Last read byte position
    session_titles: HashMap<PathBuf, String>,
    latest_sessions: HashMap<String, SessionEvent>, // SessionId -> Latest Event
    active_agy_sessions: HashMap<String, PathBuf>,
    brain_dir: PathBuf,
    presence_dir: PathBuf,
    claude_dir: PathBuf,
    distro_name: String,
}

impl TranscriptWatcher {
    pub fn new() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        let brain_dir = PathBuf::from(&home).join(".gemini/antigravity-cli/brain");
        let claude_dir = PathBuf::from(&home).join(".claude");
        let distro_name = std::env::var("WSL_DISTRO_NAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "clibox".to_string());

        Self::with_dirs(brain_dir, claude_dir, distro_name)
    }

    pub fn with_dirs(brain_dir: PathBuf, claude_dir: PathBuf, distro_name: String) -> Self {
        let presence_dir = brain_dir
            .parent()
            .map(|p| p.join("presence"))
            .unwrap_or_else(|| PathBuf::from("presence"));
        Self {
            watched_sessions: HashMap::new(),
            session_titles: HashMap::new(),
            latest_sessions: HashMap::new(),
            active_agy_sessions: HashMap::new(),
            brain_dir,
            presence_dir,
            claude_dir,
            distro_name,
        }
    }

    #[allow(dead_code)]
    pub fn with_presence_dir(mut self, presence_dir: PathBuf) -> Self {
        self.presence_dir = presence_dir;
        self
    }

    /// Returns a snapshot of all currently known active sessions
    pub fn get_latest_sessions(&self) -> Vec<SessionEvent> {
        self.latest_sessions.values().cloned().collect()
    }

    /// Scans both brain and Claude directories for any active transcripts with new events
    pub fn scan_and_collect_events(&mut self) -> Vec<SessionEvent> {
        let mut events = Vec::new();

        // 1. Scan Antigravity Brain Directory
        if self.brain_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&self.brain_dir) {
                let mut currently_active: HashMap<String, PathBuf> = HashMap::new();

                for entry in entries.flatten() {
                    let session_path = entry.path();
                    if session_path.is_dir() {
                        let session_id = session_path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("unknown")
                            .to_string();

                        if self.presence_dir.exists() && !is_session_process_active(&self.presence_dir, &session_id) {
                            continue;
                        }

                        let transcript_file =
                            session_path.join(".system_generated/logs/transcript.jsonl");
                        if transcript_file.exists() {
                            currently_active.insert(session_id.clone(), transcript_file.clone());

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

                            if let Some(event) = self.check_antigravity_transcript(
                                &session_id,
                                &session_path,
                                &transcript_file,
                            ) {
                                events.push(event);
                            }
                        }
                    }
                }

                // Detect and clean up exited sessions
                if self.presence_dir.exists() {
                    for (exited_id, path) in &self.active_agy_sessions {
                        if !currently_active.contains_key(exited_id) {
                            self.watched_sessions.remove(path);
                            self.session_titles.remove(path);
                            self.latest_sessions.remove(exited_id);

                            let exit_event = SessionEvent::new(
                                format!("wsl-{}-{}", self.distro_name, exited_id),
                                "Session Exited",
                                "Gemini",
                                AgentState::Exited,
                                "Session terminated",
                                0,
                                SessionMetadata {
                                    host: format!("wsl:{}", self.distro_name),
                                    tmux_session: None,
                                    tmux_window: None,
                                    tmux_pane: None,
                                    cwd: None,
                                    pid: None,
                                    agent_type: Some("Gemini".to_string()),
                                },
                            );
                            events.push(exit_event);
                        }
                    }
                    self.active_agy_sessions = currently_active;
                }
            }
        }

        // 2. Scan Claude Code Projects Directory
        if self.claude_dir.exists() {
            let projects_dir = self.claude_dir.join("projects");
            if let Ok(project_entries) = std::fs::read_dir(&projects_dir) {
                for project_entry in project_entries.flatten() {
                    let project_path = project_entry.path();
                    if project_path.is_dir() {
                        if let Ok(files) = std::fs::read_dir(&project_path) {
                            for file_entry in files.flatten() {
                                let path = file_entry.path();
                                if path.is_file()
                                    && path.extension().map_or(false, |ext| ext == "jsonl")
                                {
                                    if let Ok(meta) = std::fs::metadata(&path) {
                                        if let Ok(modified) = meta.modified() {
                                            if let Ok(elapsed) = modified.elapsed() {
                                                if elapsed.as_secs() > 86400 * 3 {
                                                    continue;
                                                }
                                            }
                                        }
                                    }

                                    if let Some(event) =
                                        self.check_claude_transcript(&project_path, &path)
                                    {
                                        events.push(event);
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Also check sessions/ directory under .claude if it exists
            let sessions_dir = self.claude_dir.join("sessions");
            if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() && path.extension().map_or(false, |ext| ext == "jsonl") {
                        if let Ok(meta) = std::fs::metadata(&path) {
                            if let Ok(modified) = meta.modified() {
                                if let Ok(elapsed) = modified.elapsed() {
                                    if elapsed.as_secs() > 86400 * 3 {
                                        continue;
                                    }
                                }
                            }
                        }
                        if let Some(event) = self.check_claude_transcript(&sessions_dir, &path) {
                            events.push(event);
                        }
                    }
                }
            }
        }

        events
    }

    fn check_antigravity_transcript(
        &mut self,
        session_id: &str,
        session_dir: &Path,
        path: &Path,
    ) -> Option<SessionEvent> {
        let file_meta = std::fs::metadata(path).ok()?;
        let file_len = file_meta.len();
        let last_pos = self.watched_sessions.get(path).copied().unwrap_or(0);

        if file_len <= last_pos && last_pos > 0 {
            return None;
        }

        let mut file = File::open(path).ok()?;
        let mut latest_step = None;

        let (new_pos, count) = SafeLineReader::read_new_lines(
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
        )
        .ok()?;

        self.watched_sessions.insert(path.to_path_buf(), new_pos);

        if count == 0 && latest_step.is_none() {
            return None;
        }

        let step = latest_step?;

        // Query tmux metadata
        let tmux_info = TmuxInspector::resolve_metadata(None, None);
        let metadata = SessionMetadata {
            host: format!("wsl:{}", self.distro_name),
            tmux_session: tmux_info.as_ref().map(|(s, _, _)| s.clone()),
            tmux_window: tmux_info.as_ref().map(|(_, w, _)| w.clone()),
            tmux_pane: tmux_info.as_ref().map(|(_, _, p)| p.clone()),
            cwd: None,
            pid: None,
            agent_type: Some("Gemini".to_string()),
        };

        let current_title = self.session_titles.get(path).cloned();
        let is_placeholder = current_title
            .as_ref()
            .map_or(true, |t| t.starts_with("Session "));

        let title = if is_placeholder {
            let upgraded = if let Some(heading) = extract_earliest_markdown_heading(session_dir) {
                heading
            } else if let Some(workdir) = extract_workdir_basename(path) {
                workdir
            } else if let Some(prompt) = extract_prompt_fallback(path) {
                prompt
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
            step.state,
            step.status_text,
            step.step_index,
            metadata,
        );

        self.latest_sessions
            .insert(session_id.to_string(), event.clone());
        Some(event)
    }

    fn check_claude_transcript(&mut self, project_dir: &Path, path: &Path) -> Option<SessionEvent> {
        let file_meta = std::fs::metadata(path).ok()?;
        let file_len = file_meta.len();
        let last_pos = self.watched_sessions.get(path).copied().unwrap_or(0);

        if file_len <= last_pos && last_pos > 0 {
            return None;
        }

        let mut file = File::open(path).ok()?;
        let mut latest_step = None;
        let mut step_count = 0u32;

        let (new_pos, count) = SafeLineReader::read_new_lines(
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
        )
        .ok()?;

        self.watched_sessions.insert(path.to_path_buf(), new_pos);

        if count == 0 && latest_step.is_none() {
            return None;
        }

        let step = latest_step?;
        let raw_session_id = step.session_id.clone().unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string()
        });

        let current_title = self.session_titles.get(path).cloned();
        let is_placeholder = current_title
            .as_ref()
            .map_or(true, |t| t.starts_with("Claude ") || t == "Claude Session");

        let title = if is_placeholder {
            let upgraded = extract_claude_title(path, project_dir);
            self.session_titles.insert(path.to_path_buf(), upgraded.clone());
            upgraded
        } else {
            current_title.unwrap()
        };

        let tmux_info = TmuxInspector::resolve_metadata(None, None);
        let metadata = SessionMetadata {
            host: format!("wsl:{}", self.distro_name),
            tmux_session: tmux_info.as_ref().map(|(s, _, _)| s.clone()),
            tmux_window: tmux_info.as_ref().map(|(_, w, _)| w.clone()),
            tmux_pane: tmux_info.as_ref().map(|(_, _, p)| p.clone()),
            cwd: step.cwd,
            pid: None,
            agent_type: Some("Claude".to_string()),
        };

        let event = SessionEvent::new(
            format!("wsl-{}-claude-{}", self.distro_name, raw_session_id),
            title,
            "Claude",
            step.state,
            step.status_text,
            step.step_index,
            metadata,
        );

        self.latest_sessions
            .insert(format!("claude-{}", raw_session_id), event.clone());
        Some(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_deck_core::AgentState;
    use std::io::Write;

    #[test]
    fn test_daemon_transcript_watcher_antigravity_and_claude() {
        let temp_dir = std::env::temp_dir().join(format!(
            "agent_deck_daemon_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let brain_dir = temp_dir.join("brain");
        let claude_dir = temp_dir.join(".claude");

        let agy_session_dir = brain_dir.join("test-session-1");
        let agy_logs_dir = agy_session_dir.join(".system_generated/logs");
        std::fs::create_dir_all(&agy_logs_dir).unwrap();

        let agy_transcript = agy_logs_dir.join("transcript.jsonl");
        {
            let mut f = File::create(&agy_transcript).unwrap();
            writeln!(
                f,
                "{{\"step_index\": 1, \"type\": \"PLANNER_RESPONSE\", \"status\": \"RUNNING\", \"tool_calls\": [{{\"name\": \"run_command\", \"args\": {{\"toolSummary\": \"Building daemon\"}}}}]}}"
            )
            .unwrap();
        }

        let claude_project_dir = claude_dir.join("projects/C--Users-test-project");
        std::fs::create_dir_all(&claude_project_dir).unwrap();
        let claude_transcript = claude_project_dir.join("test-claude-session.jsonl");
        {
            let mut f = File::create(&claude_transcript).unwrap();
            writeln!(
                f,
                "{{\"type\": \"assistant\", \"sessionId\": \"test-claude-session\", \"message\": {{\"role\": \"assistant\", \"content\": [{{\"type\": \"tool_use\", \"name\": \"Bash\", \"input\": {{\"command\": \"cargo check\"}}}}], \"stop_reason\": \"tool_use\"}}}}"
            )
            .unwrap();
        }

        let mut watcher =
            TranscriptWatcher::with_dirs(brain_dir, claude_dir, "test-distro".to_string());
        let events = watcher.scan_and_collect_events();

        assert_eq!(events.len(), 2);

        let agy_event = events.iter().find(|e| e.agent_type == "Gemini").unwrap();
        assert!(matches!(agy_event.state, AgentState::RunningTool { .. }));
        assert_eq!(agy_event.metadata.host, "wsl:test-distro");

        let claude_event = events.iter().find(|e| e.agent_type == "Claude").unwrap();
        assert!(matches!(claude_event.state, AgentState::RunningTool { .. }));
        assert_eq!(claude_event.metadata.agent_type, Some("Claude".to_string()));

        let latest = watcher.get_latest_sessions();
        assert_eq!(latest.len(), 2);

        // Clean up
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_daemon_transcript_watcher_empty_dirs() {
        let temp_dir = std::env::temp_dir().join(format!(
            "agent_deck_daemon_empty_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let brain_dir = temp_dir.join("nonexistent_brain");
        let claude_dir = temp_dir.join("nonexistent_claude");

        let mut watcher = TranscriptWatcher::with_dirs(brain_dir, claude_dir, "empty-distro".to_string());
        let events = watcher.scan_and_collect_events();
        assert_eq!(events.len(), 0);
        assert_eq!(watcher.get_latest_sessions().len(), 0);
    }

    #[test]
    fn test_daemon_transcript_watcher_incremental_events() {
        let temp_dir = std::env::temp_dir().join(format!(
            "agent_deck_daemon_inc_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let brain_dir = temp_dir.join("brain");
        let claude_dir = temp_dir.join(".claude");

        let agy_session_dir = brain_dir.join("inc-session-1");
        let agy_logs_dir = agy_session_dir.join(".system_generated/logs");
        std::fs::create_dir_all(&agy_logs_dir).unwrap();

        let agy_transcript = agy_logs_dir.join("transcript.jsonl");
        {
            let mut f = File::create(&agy_transcript).unwrap();
            writeln!(
                f,
                "{{\"step_index\": 1, \"type\": \"PLANNER_RESPONSE\", \"status\": \"RUNNING\", \"tool_calls\": [{{\"name\": \"run_command\", \"args\": {{\"toolSummary\": \"Step 1\"}}}}]}}"
            )
            .unwrap();
        }

        let mut watcher = TranscriptWatcher::with_dirs(brain_dir, claude_dir, "inc-distro".to_string());
        let events1 = watcher.scan_and_collect_events();
        assert_eq!(events1.len(), 1);
        assert_eq!(events1[0].step_count, 1);

        // Append Step 2
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&agy_transcript).unwrap();
            writeln!(
                f,
                "{{\"step_index\": 2, \"type\": \"PLANNER_RESPONSE\", \"status\": \"DONE\"}}"
            )
            .unwrap();
        }

        let events2 = watcher.scan_and_collect_events();
        assert_eq!(events2.len(), 1);
        assert_eq!(events2[0].step_count, 2);
        assert!(matches!(events2[0].state, AgentState::WaitingForInput { .. }));

        // Clean up
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_daemon_transcript_watcher_session_exit_cleanup() {
        let temp_dir = std::env::temp_dir().join(format!(
            "agent_deck_daemon_exit_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let brain_dir = temp_dir.join("brain");
        let presence_dir = temp_dir.join("presence");
        let claude_dir = temp_dir.join(".claude");
        std::fs::create_dir_all(&presence_dir).unwrap();

        let session_id = "test-exit-session";
        let agy_session_dir = brain_dir.join(session_id);
        let agy_logs_dir = agy_session_dir.join(".system_generated/logs");
        std::fs::create_dir_all(&agy_logs_dir).unwrap();

        let agy_transcript = agy_logs_dir.join("transcript.jsonl");
        {
            let mut f = File::create(&agy_transcript).unwrap();
            writeln!(
                f,
                "{{\"step_index\": 1, \"type\": \"PLANNER_RESPONSE\", \"status\": \"RUNNING\", \"tool_calls\": [{{\"name\": \"run_command\", \"args\": {{\"toolSummary\": \"Building daemon\"}}}}]}}"
            )
            .unwrap();
        }

        // Create and hold lock file exclusively (simulating running agy process)
        let lock_file = presence_dir.join(format!("{}.lock", session_id));
        #[cfg(target_os = "windows")]
        use std::os::windows::fs::OpenOptionsExt;
        let held_lock = {
            let mut opts = std::fs::OpenOptions::new();
            opts.read(true).write(true).create(true).truncate(true);
            #[cfg(target_os = "windows")]
            opts.share_mode(0);
            opts.open(&lock_file).unwrap()
        };
        #[cfg(unix)]
        unsafe {
            use std::os::unix::io::AsRawFd;
            libc::flock(held_lock.as_raw_fd(), libc::LOCK_EX);
        }

        let mut watcher = TranscriptWatcher::with_dirs(brain_dir, claude_dir, "test-distro".to_string())
            .with_presence_dir(presence_dir);

        let events1 = watcher.scan_and_collect_events();
        assert_eq!(events1.len(), 1);
        assert_eq!(events1[0].agent_type, "Gemini");
        assert_eq!(watcher.get_latest_sessions().len(), 1);

        // Simulate session termination: release lock and remove file
        drop(held_lock);
        let _ = std::fs::remove_file(&lock_file);

        let events2 = watcher.scan_and_collect_events();
        assert_eq!(events2.len(), 1);
        assert_eq!(events2[0].state, AgentState::Exited);
        assert_eq!(events2[0].session_id, "wsl-test-distro-test-exit-session");

        // Latest sessions snapshot should now be empty
        assert_eq!(watcher.get_latest_sessions().len(), 0);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
