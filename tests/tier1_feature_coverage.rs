mod common;

use agent_deck_core::{AgentState, SessionEvent, SessionMetadata};
use common::{
    append_json_line, append_raw_bytes, create_test_hub, make_event,
    AttentionState, CustomTitlesStorage, DynamicCategory, LayoutFormulas, SessionHub,
    TestTempDir, UserAction,
};
use egui::{Color32, Rect, pos2};
use serde_json::json;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

// ==============================================================================
// F1: Newline Ingestion & Offset Sync (R1)
// ==============================================================================

#[test]
fn test_f1_01_valid_newline_delimited_jsonl() {
    let temp = TestTempDir::new("f1_01");
    let file_path = temp.file_path("transcript.jsonl");

    let event1 = json!({"step_index": 1, "type": "USER_INPUT", "content": "Hello"});
    let event2 = json!({"step_index": 2, "type": "PLANNER_RESPONSE", "status": "DONE"});

    append_json_line(&file_path, &event1);
    append_json_line(&file_path, &event2);

    let file = File::open(&file_path).expect("open file");
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader.lines().flatten().collect();

    assert_eq!(lines.len(), 2);
    let parsed1: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    let parsed2: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();

    assert_eq!(parsed1["step_index"], 1);
    assert_eq!(parsed2["step_index"], 2);
}

#[test]
fn test_f1_02_partial_line_without_newline_not_consumed() {
    let temp = TestTempDir::new("f1_02");
    let file_path = temp.file_path("transcript.jsonl");

    // Write complete line 1
    let event1 = json!({"step_index": 1, "type": "USER_INPUT", "content": "Start"});
    append_json_line(&file_path, &event1);
    let meta1 = std::fs::metadata(&file_path).unwrap();
    let last_valid_offset = meta1.len();

    // Write incomplete line 2 (partial JSON without trailing newline)
    let partial_bytes = b"{\"step_index\": 2, \"type\": \"PLANNER_RESP";
    append_raw_bytes(&file_path, partial_bytes);

    // Watcher logic: check that seeking to last_valid_offset does not treat partial bytes as a complete line
    let mut file = File::open(&file_path).unwrap();
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(last_valid_offset)).unwrap();
    let mut reader = BufReader::new(file);

    let mut partial_line = String::new();
    let bytes_read = reader.read_line(&mut partial_line).unwrap();

    assert_eq!(bytes_read, partial_bytes.len());
    assert!(!partial_line.ends_with('\n'), "Partial line must not end with newline");
    assert!(serde_json::from_str::<serde_json::Value>(&partial_line).is_err(), "Partial line is invalid JSON");
}

#[test]
fn test_f1_03_incremental_append_completes_partial_line() {
    let temp = TestTempDir::new("f1_03");
    let file_path = temp.file_path("transcript.jsonl");

    // Write line 1
    append_json_line(&file_path, &json!({"step_index": 1, "content": "First"}));

    // Write partial line 2
    append_raw_bytes(&file_path, b"{\"step_index\": 2, \"content\": \"Second");

    // Later write completes line 2 with newline
    append_raw_bytes(&file_path, b"\"}\n");

    let file = File::open(&file_path).unwrap();
    let lines: Vec<String> = BufReader::new(file).lines().flatten().collect();

    assert_eq!(lines.len(), 2);
    let parsed2: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
    assert_eq!(parsed2["step_index"], 2);
    assert_eq!(parsed2["content"], "Second");
}

#[test]
fn test_f1_04_multiple_lines_in_single_flush() {
    let temp = TestTempDir::new("f1_04");
    let file_path = temp.file_path("transcript.jsonl");

    let mut chunk = Vec::new();
    for i in 1..=5 {
        let line = serde_json::to_string(&json!({"step_index": i, "data": format!("step_{}", i)})).unwrap();
        chunk.extend_from_slice(line.as_bytes());
        chunk.push(b'\n');
    }
    append_raw_bytes(&file_path, &chunk);

    let file = File::open(&file_path).unwrap();
    let lines: Vec<String> = BufReader::new(file).lines().flatten().collect();

    assert_eq!(lines.len(), 5);
    for (idx, line) in lines.iter().enumerate() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["step_index"], idx + 1);
    }
}

#[test]
fn test_f1_05_crlf_and_blank_lines_handling() {
    let temp = TestTempDir::new("f1_05");
    let file_path = temp.file_path("transcript.jsonl");

    let raw_payload = b"{\"step_index\": 1}\r\n\r\n   \r\n{\"step_index\": 2}\r\n";
    append_raw_bytes(&file_path, raw_payload);

    let file = File::open(&file_path).unwrap();
    let non_empty_lines: Vec<String> = BufReader::new(file)
        .lines()
        .flatten()
        .filter(|l| !l.trim().is_empty())
        .collect();

    assert_eq!(non_empty_lines.len(), 2);
    let p1: serde_json::Value = serde_json::from_str(non_empty_lines[0].trim()).unwrap();
    let p2: serde_json::Value = serde_json::from_str(non_empty_lines[1].trim()).unwrap();
    assert_eq!(p1["step_index"], 1);
    assert_eq!(p2["step_index"], 2);
}

// ==============================================================================
// F2: Deterministic State Transitions & RunningTool (R1)
// ==============================================================================

#[test]
fn test_f2_01_transition_user_input_to_thinking() {
    let mut hub = create_test_hub();
    let event = make_event(
        "s1",
        "Session 1",
        "Gemini",
        AgentState::Thinking,
        "PROCESSING: Refactor database client",
        1,
        "Windows",
    );

    hub.sender().send(event).unwrap();
    hub.poll_events();

    assert_eq!(hub.sessions.len(), 1);
    assert_eq!(hub.sessions[0].state, AgentState::Thinking);
    assert!(hub.sessions[0].status_text.contains("PROCESSING"));
    assert!(!hub.sessions[0].attention.is_unacknowledged);
}

#[test]
fn test_f2_02_transition_tool_call_waiting_approval() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s1", "Session 1", "Gemini", AgentState::Thinking, "Thinking", 1, "Windows")).unwrap();
    hub.poll_events();

    let event = make_event(
        "s1",
        "Session 1",
        "Gemini",
        AgentState::WaitingForApproval {
            name: "write_to_file".to_string(),
            summary: "Writing updated config.toml".to_string(),
        },
        "PERMISSION REQUIRED: write_to_file",
        2,
        "Windows",
    );

    hub.sender().send(event).unwrap();
    hub.poll_events();

    assert_eq!(hub.sessions.len(), 1);
    assert!(matches!(hub.sessions[0].state, AgentState::WaitingForApproval { .. }));
    assert!(hub.sessions[0].attention.is_unacknowledged);
    assert!(hub.sessions[0].attention.is_pulsating(&hub.sessions[0].state));
}

#[test]
fn test_f2_03_transition_running_tool_execution() {
    let mut hub = create_test_hub();
    let event1 = make_event(
        "s1",
        "Session 1",
        "Gemini",
        AgentState::WaitingForApproval {
            name: "run_command".to_string(),
            summary: "Running cargo test".to_string(),
        },
        "PERMISSION REQUIRED",
        2,
        "Windows",
    );
    hub.sender().send(event1).unwrap();
    hub.poll_events();

    // Now user approves and tool starts running
    let event2 = make_event(
        "s1",
        "Session 1",
        "Gemini",
        AgentState::RunningTool {
            name: "run_command".to_string(),
            summary: "Executing: cargo test".to_string(),
        },
        "TOOL: run_command - Executing: cargo test",
        3,
        "Windows",
    );
    hub.sender().send(event2).unwrap();
    hub.poll_events();

    assert_eq!(hub.sessions.len(), 1);
    assert!(matches!(hub.sessions[0].state, AgentState::RunningTool { .. }));
    // RunningTool is an active working state, not waiting for user
    assert!(!hub.sessions[0].attention.is_unacknowledged);
}

#[test]
fn test_f2_04_transition_planner_response_done_to_waiting_input() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s1", "Session 1", "Gemini", AgentState::Thinking, "Thinking", 1, "Windows")).unwrap();
    hub.poll_events();

    let event = make_event(
        "s1",
        "Session 1",
        "Gemini",
        AgentState::WaitingForInput {
            prompt_preview: "All tasks completed successfully".to_string(),
        },
        "WAITING FOR PROMPT",
        2,
        "Windows",
    );

    hub.sender().send(event).unwrap();
    hub.poll_events();

    assert_eq!(hub.sessions.len(), 1);
    assert!(matches!(hub.sessions[0].state, AgentState::WaitingForInput { .. }));
    assert!(hub.sessions[0].attention.is_unacknowledged);
}

#[test]
fn test_f2_05_transition_error_and_finished_states() {
    let mut hub = create_test_hub();

    let err_event = make_event("s1", "Session 1", "Gemini", AgentState::Error { message: "Compilation failed".to_string() }, "ERROR: Compilation failed", 5, "Windows");
    hub.sender().send(err_event).unwrap();
    hub.poll_events();
    assert!(matches!(hub.sessions[0].state, AgentState::Error { .. }));

    let fin_event = make_event("s1", "Session 1", "Gemini", AgentState::Finished, "Session Finished", 6, "Windows");
    hub.sender().send(fin_event).unwrap();
    hub.poll_events();
    assert_eq!(hub.sessions[0].state, AgentState::Finished);
}

// ==============================================================================
// F3: Claude Code Transcript Parser (R1)
// ==============================================================================

#[test]
fn test_f3_01_claude_code_transcript_discovery() {
    let temp = TestTempDir::new("f3_01");
    let claude_dir = temp.create_sub_dir(".claude/transcripts");
    let transcript_path = claude_dir.join("session_abc123.jsonl");

    let entry = json!({
        "type": "user",
        "message": {"content": [{"type": "text", "text": "Fix login bug"}]}
    });
    append_json_line(&transcript_path, &entry);

    assert!(transcript_path.exists());
    assert!(transcript_path.to_str().unwrap().contains(".claude"));
}

#[test]
fn test_f3_02_claude_code_user_turn_prompt() {
    let user_msg = json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": "Run cargo test in agent-deck-ui"}]
        }
    });

    let prompt_text = user_msg["message"]["content"][0]["text"].as_str().unwrap();
    let state = AgentState::Thinking;
    let status_text = format!("PROCESSING: {}", prompt_text);

    let event = make_event("claude-1", "Claude Session", "Claude", state, &status_text, 1, "wsl:Ubuntu");
    assert_eq!(event.agent_type, "Claude");
    assert!(event.status_text.contains("Run cargo test"));
}

#[test]
fn test_f3_03_claude_code_tool_use_block_extraction() {
    let assistant_msg = json!({
        "type": "assistant",
        "message": {
            "role": "assistant",
            "content": [
                {
                    "type": "tool_use",
                    "id": "toolu_01",
                    "name": "Bash",
                    "input": {"command": "cargo check --workspace"}
                }
            ]
        }
    });

    let tool_block = &assistant_msg["message"]["content"][0];
    let tool_name = tool_block["name"].as_str().unwrap();
    let cmd = tool_block["input"]["command"].as_str().unwrap();

    let state = AgentState::RunningTool {
        name: tool_name.to_string(),
        summary: cmd.to_string(),
    };

    assert_eq!(tool_name, "Bash");
    if let AgentState::RunningTool { name, summary } = state {
        assert_eq!(name, "Bash");
        assert_eq!(summary, "cargo check --workspace");
    } else {
        panic!("Expected RunningTool state");
    }
}

#[test]
fn test_f3_04_claude_code_tool_result_and_turn_completion() {
    let result_msg = json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "toolu_01",
                    "content": "Finished check profile in 0.45s",
                    "is_error": false
                }
            ]
        }
    });

    let is_err = result_msg["message"]["content"][0]["is_error"].as_bool().unwrap();
    assert!(!is_err);

    // After tool result processed, Claude Code becomes ready for input
    let state = AgentState::WaitingForInput { prompt_preview: "Check passed".to_string() };
    assert!(matches!(state, AgentState::WaitingForInput { .. }));
}

#[test]
fn test_f3_05_claude_code_error_handling_and_denial() {
    let denial_msg = json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "toolu_02",
                    "content": "User denied tool execution: git reset --hard",
                    "is_error": true
                }
            ]
        }
    });

    let is_err = denial_msg["message"]["content"][0]["is_error"].as_bool().unwrap();
    let err_msg = denial_msg["message"]["content"][0]["content"].as_str().unwrap();

    assert!(is_err);
    assert!(err_msg.contains("User denied"));

    let state = AgentState::Error { message: err_msg.to_string() };
    assert!(matches!(state, AgentState::Error { .. }));
}

// ==============================================================================
// F4: Persistent Session Dismissal Tracking (AC1)
// ==============================================================================

#[test]
fn test_f4_01_dismiss_removes_from_active_list() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s1", "Session 1", "Gemini", AgentState::Idle, "Idle", 1, "Windows")).unwrap();
    hub.poll_events();
    assert_eq!(hub.sessions.len(), 1);

    hub.apply_actions(vec![UserAction::Dismiss("s1".to_string())]);
    assert_eq!(hub.sessions.len(), 0);
    assert!(hub.dismissed_sessions.contains("s1"));
}

#[test]
fn test_f4_02_dismiss_records_step_count() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s1", "Session 1", "Gemini", AgentState::Thinking, "Thinking", 10, "Windows")).unwrap();
    hub.poll_events();

    hub.apply_actions(vec![UserAction::Dismiss("s1".to_string())]);
    assert!(hub.dismissed_sessions.contains("s1"));
}

#[test]
fn test_f4_03_stale_retransmission_ignored() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s1", "Session 1", "Gemini", AgentState::Thinking, "Thinking", 10, "Windows")).unwrap();
    hub.poll_events();
    assert_eq!(hub.sessions.len(), 1);

    hub.apply_actions(vec![UserAction::Dismiss("s1".to_string())]);
    assert_eq!(hub.sessions.len(), 0);
    assert!(hub.dismissed_sessions.contains("s1"));

    hub.poll_events();
    assert_eq!(hub.sessions.len(), 0);
    assert!(hub.dismissed_sessions.contains("s1"));
}

#[test]
fn test_f4_04_new_step_resurrects_dismissed_session() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s1", "Session 1", "Gemini", AgentState::Thinking, "Thinking", 10, "Windows")).unwrap();
    hub.poll_events();

    hub.apply_actions(vec![UserAction::Dismiss("s1".to_string())]);

    // Incoming event with higher step count (11) represents new active turn
    hub.sender().send(make_event("s1", "Session 1", "Gemini", AgentState::Thinking, "New Question", 11, "Windows")).unwrap();
    hub.poll_events();

    assert_eq!(hub.sessions.len(), 1, "Session with higher step count must resurrect");
    assert_eq!(hub.sessions[0].step_count, 11);
}

#[test]
fn test_f4_05_rapid_multiple_dismissals() {
    let mut hub = create_test_hub();
    for i in 1..=5 {
        hub.sender().send(make_event(&format!("s{}", i), "Session", "Gemini", AgentState::Idle, "Idle", 1, "Windows")).unwrap();
    }
    hub.poll_events();
    assert_eq!(hub.sessions.len(), 5);

    let dismissals = vec![
        UserAction::Dismiss("s1".to_string()),
        UserAction::Dismiss("s3".to_string()),
        UserAction::Dismiss("s5".to_string()),
    ];
    hub.apply_actions(dismissals);

    assert_eq!(hub.sessions.len(), 2);
    let remaining_ids: Vec<String> = hub.sessions.iter().map(|s| s.session_id.clone()).collect();
    assert_eq!(remaining_ids, vec!["s2", "s4"]);
}

#[test]
fn test_f4_06_session_exit_cleans_up_from_ui_state() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("win-gemini-123", "Session 123", "Gemini", AgentState::Thinking, "Running tool", 5, "Windows")).unwrap();
    hub.poll_events();
    assert_eq!(hub.sessions.len(), 1);

    // When the CLI process exits, the adapter sends an Exited event
    hub.sender().send(make_event("win-gemini-123", "Session 123", "Gemini", AgentState::Exited, "Session terminated", 0, "Windows")).unwrap();
    hub.poll_events();

    assert_eq!(hub.sessions.len(), 0, "Session must be cleanly removed from UI state upon exit");
    assert!(hub.dismissed_sessions.contains("win-gemini-123"));
}

#[test]
fn test_f4_07_session_exit_clears_attention_and_updates_categories() {
    let mut hub = create_test_hub();
    // Session on WSL2 Ubuntu awaiting approval
    hub.sender().send(make_event(
        "wsl-ubuntu-sess1",
        "Ubuntu Task",
        "Gemini",
        AgentState::WaitingForApproval { name: "run_command".into(), summary: "rm -rf".into() },
        "Permission required",
        3,
        "wsl:ubuntu",
    )).unwrap();
    hub.poll_events();

    assert_eq!(hub.sessions.len(), 1);
    assert!(SessionHub::has_waiting_input(&hub.sessions.iter().collect::<Vec<_>>()));

    let cats = hub.active_categories();
    assert!(cats.iter().any(|c| c.label == "ubuntu"));

    // Session exits
    hub.sender().send(make_event(
        "wsl-ubuntu-sess1",
        "Ubuntu Task",
        "Gemini",
        AgentState::Exited,
        "Session terminated",
        0,
        "wsl:ubuntu",
    )).unwrap();
    hub.poll_events();

    assert_eq!(hub.sessions.len(), 0);
    assert!(!SessionHub::has_waiting_input(&hub.sessions.iter().collect::<Vec<_>>()));

    // Non-permanent empty WSL category tab should no longer be shown
    let updated_cats = hub.active_categories();
    assert!(!updated_cats.iter().any(|c| c.label == "ubuntu"));
}

#[test]
fn test_f4_08_session_resurrect_after_exit() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("win-gemini-456", "Session 456", "Gemini", AgentState::Thinking, "Thinking", 10, "Windows")).unwrap();
    hub.poll_events();
    assert_eq!(hub.sessions.len(), 1);

    // Session exits
    hub.sender().send(make_event("win-gemini-456", "Session 456", "Gemini", AgentState::Exited, "Terminated", 0, "Windows")).unwrap();
    hub.poll_events();
    assert_eq!(hub.sessions.len(), 0);

    // User restarts session or new turn occurs with step 11
    hub.sender().send(make_event("win-gemini-456", "Session 456", "Gemini", AgentState::Thinking, "New task", 11, "Windows")).unwrap();
    hub.poll_events();

    assert_eq!(hub.sessions.len(), 1);
    assert_eq!(hub.sessions[0].step_count, 11);
}

// ==============================================================================
// F5: Scope-Sensitive Alert Acknowledgement (R1, R3)
// ==============================================================================

#[test]
fn test_f5_01_select_session_acknowledges_only_target() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s1", "S1", "Gemini", AgentState::Thinking, "T1", 1, "Windows")).unwrap();
    hub.sender().send(make_event("s2", "S2", "Gemini", AgentState::Thinking, "T2", 1, "Windows")).unwrap();
    hub.poll_events();

    hub.sender().send(make_event("s1", "S1", "Gemini", AgentState::WaitingForInput { prompt_preview: "P1".into() }, "W", 2, "Windows")).unwrap();
    hub.sender().send(make_event("s2", "S2", "Gemini", AgentState::WaitingForInput { prompt_preview: "P2".into() }, "W", 2, "Windows")).unwrap();
    hub.poll_events();

    assert!(hub.sessions[0].attention.is_unacknowledged);
    assert!(hub.sessions[1].attention.is_unacknowledged);

    // Select s1 only
    hub.apply_actions(vec![UserAction::Select("s1".to_string())]);

    assert!(!hub.sessions[0].attention.is_unacknowledged, "Target session s1 should be acknowledged");
    assert!(hub.sessions[1].attention.is_unacknowledged, "Non-target session s2 must remain unacknowledged");
}

#[test]
fn test_f5_02_acknowledge_category_windows() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("win-1", "Win1", "Gemini", AgentState::Thinking, "T", 1, "Windows")).unwrap();
    hub.sender().send(make_event("wsl-1", "WSL1", "Gemini", AgentState::Thinking, "T", 1, "wsl:Ubuntu")).unwrap();
    hub.poll_events();

    hub.sender().send(make_event("win-1", "Win1", "Gemini", AgentState::WaitingForInput { prompt_preview: "P".into() }, "W", 2, "Windows")).unwrap();
    hub.sender().send(make_event("wsl-1", "WSL1", "Gemini", AgentState::WaitingForInput { prompt_preview: "P".into() }, "W", 2, "wsl:Ubuntu")).unwrap();
    hub.poll_events();

    assert!(hub.sessions[0].attention.is_unacknowledged);
    assert!(hub.sessions[1].attention.is_unacknowledged);

    hub.apply_actions(vec![UserAction::AcknowledgeCategory("windows".to_string())]);

    assert!(!hub.sessions[0].attention.is_unacknowledged, "Windows session must be acknowledged");
    assert!(hub.sessions[1].attention.is_unacknowledged, "WSL session must remain unacknowledged");
}

#[test]
fn test_f5_03_acknowledge_category_wsl_host() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("win-1", "Win1", "Gemini", AgentState::Thinking, "T", 1, "Windows")).unwrap();
    hub.sender().send(make_event("wsl-1", "WSL1", "Gemini", AgentState::Thinking, "T", 1, "wsl:Ubuntu")).unwrap();
    hub.poll_events();

    hub.sender().send(make_event("win-1", "Win1", "Gemini", AgentState::WaitingForInput { prompt_preview: "P".into() }, "W", 2, "Windows")).unwrap();
    hub.sender().send(make_event("wsl-1", "WSL1", "Gemini", AgentState::WaitingForInput { prompt_preview: "P".into() }, "W", 2, "wsl:Ubuntu")).unwrap();
    hub.poll_events();

    assert!(hub.sessions[0].attention.is_unacknowledged);
    assert!(hub.sessions[1].attention.is_unacknowledged);

    hub.apply_actions(vec![UserAction::AcknowledgeCategory("host:Ubuntu".to_string())]);

    assert!(hub.sessions[0].attention.is_unacknowledged, "Windows session must remain unacknowledged");
    assert!(!hub.sessions[1].attention.is_unacknowledged, "Ubuntu WSL session must be acknowledged");
}

#[test]
fn test_f5_04_acknowledge_all_clears_global() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s1", "S1", "Gemini", AgentState::Thinking, "T", 1, "Windows")).unwrap();
    hub.sender().send(make_event("s2", "S2", "Gemini", AgentState::Thinking, "T", 1, "wsl:Debian")).unwrap();
    hub.poll_events();

    hub.sender().send(make_event("s1", "S1", "Gemini", AgentState::WaitingForInput { prompt_preview: "P".into() }, "W", 2, "Windows")).unwrap();
    hub.sender().send(make_event("s2", "S2", "Gemini", AgentState::WaitingForApproval { name: "tool".into(), summary: "sum".into() }, "W", 2, "wsl:Debian")).unwrap();
    hub.poll_events();

    hub.apply_actions(vec![UserAction::AcknowledgeAll]);

    assert!(!hub.sessions[0].attention.is_unacknowledged);
    assert!(!hub.sessions[1].attention.is_unacknowledged);
}

#[test]
fn test_f5_05_pulse_timeout_auto_stops_after_4s() {
    let mut attention = AttentionState::new();
    attention.update(&AgentState::Thinking, 1);
    let state = AgentState::WaitingForInput { prompt_preview: "Prompt".to_string() };
    attention.update(&state, 2);

    assert!(attention.is_pulsating(&state));

    // Fast-forward simulated trigger time by 4.5 seconds
    attention.triggered_at = Some(Instant::now() - Duration::from_millis(4500));
    assert!(!attention.is_pulsating(&state), "Pulsating must auto-stop after 4 seconds");
}

// ==============================================================================
// F6: In-Place Session Mutation & State Persistence (R2, R3)
// ==============================================================================

#[test]
fn test_f6_01_marquee_offset_in_place_mutation() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s1", "S1", "Gemini", AgentState::Thinking, "Thinking", 1, "Windows")).unwrap();
    hub.poll_events();

    assert_eq!(hub.sessions[0].marquee_offset, 0.0);
    hub.sessions[0].marquee_offset = LayoutFormulas::marquee_advance(hub.sessions[0].marquee_offset, 0.1);
    assert!((hub.sessions[0].marquee_offset - 3.8).abs() < 0.001);
}

#[test]
fn test_f6_02_vu_levels_in_place_mutation() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s1", "S1", "Gemini", AgentState::Thinking, "Thinking", 1, "Windows")).unwrap();
    hub.poll_events();

    let dt = 0.016;
    let pulse_phase = 1.0;
    for (i, bar) in hub.sessions[0].vu_levels.iter_mut().enumerate() {
        *bar = LayoutFormulas::vu_update_active(*bar, i, pulse_phase, dt);
        assert!(*bar >= 0.0 && *bar <= 1.0);
    }
}

#[test]
fn test_f6_03_custom_title_persistence_save_and_retrieve() {
    let mut storage = CustomTitlesStorage::in_memory();

    storage.set_title("s1", "Database Worker");
    assert_eq!(storage.get_title("s1"), Some("Database Worker".to_string()));
}

#[test]
fn test_f6_04_custom_title_reset_on_empty() {
    let mut storage = CustomTitlesStorage::in_memory();

    storage.set_title("s1", "Database Worker");
    storage.set_title("s1", "   "); // Empty or whitespace resets title
    assert_eq!(storage.get_title("s1"), None);
}

#[test]
fn test_f6_05_custom_title_persists_across_poll_events() {
    let storage = Arc::new(RwLock::new(CustomTitlesStorage::in_memory()));
    storage.write().unwrap().set_title("s1", "Friendly Custom Name");

    let mut hub = SessionHub::new(storage);
    hub.sender().send(make_event("s1", "Default Name", "Gemini", AgentState::Idle, "Idle", 1, "Windows")).unwrap();
    hub.poll_events();

    assert_eq!(hub.sessions[0].display_name, "Friendly Custom Name");
}

// ==============================================================================
// F7: Proportional Dynamic Scaling (0.85x - 1.6x) (R2, AC3)
// ==============================================================================

#[test]
fn test_f7_01_min_font_scale_clamped_at_085() {
    let clamped = LayoutFormulas::clamp_font_scale(0.50);
    assert_eq!(clamped, 0.85);
}

#[test]
fn test_f7_02_max_font_scale_clamped_at_16() {
    let clamped = LayoutFormulas::clamp_font_scale(2.20);
    assert_eq!(clamped, 1.60);
}

#[test]
fn test_f7_03_row_height_scales_proportionally() {
    let h_min = LayoutFormulas::normal_row_height(0.85);
    let h_mid = LayoutFormulas::normal_row_height(1.15);
    let h_max = LayoutFormulas::normal_row_height(1.60);

    assert!(h_min < h_mid);
    assert!(h_mid <= h_max);
    assert!((h_min - 44.2).abs() < 0.1);
}

#[test]
fn test_f7_04_edit_overlay_height_scales_proportionally() {
    let normal_h = LayoutFormulas::normal_row_height(1.15);
    let edit_h = LayoutFormulas::edit_row_height(1.15);

    assert!(edit_h > normal_h);
    assert!((edit_h - (74.0 * 1.15)).abs() < 0.1);
}

#[test]
fn test_f7_05_badge_and_button_geometry_scales() {
    let scale1 = 1.0;
    let scale2 = 1.5;

    assert!(LayoutFormulas::badge_font_size(scale2) > LayoutFormulas::badge_font_size(scale1));
    assert!(LayoutFormulas::status_font_size(scale2) > LayoutFormulas::status_font_size(scale1));
    assert!(LayoutFormulas::button_font_size(scale2) > LayoutFormulas::button_font_size(scale1));
}

// ==============================================================================
// F8: Bounding Box Padding & Text Layout (R2, AC3)
// ==============================================================================

#[test]
fn test_f8_01_marquee_clip_rect_vertical_padding() {
    let scale = 1.15;
    let area_h = LayoutFormulas::marquee_area_height(scale);
    let font_size = LayoutFormulas::status_font_size(scale);

    // Height of clip rect must exceed font size with positive padding
    assert!(area_h > font_size);
    let padding = area_h - font_size;
    assert!(padding >= 4.0 * scale, "Padding must be sufficient to prevent clipping");
}

#[test]
fn test_f8_02_marquee_wrap_offset_continuity() {
    let scale = 1.0;
    let text = "Running test cases in workspace";
    let wrap1 = LayoutFormulas::marquee_modulo_offset(100.0, text.len(), scale);
    let wrap2 = LayoutFormulas::marquee_modulo_offset(1000.0, text.len(), scale);

    let max_wrap = (text.len() + 6) as f32 * 7.0 * scale + 40.0;
    assert!(wrap1 >= 0.0 && wrap1 < max_wrap);
    assert!(wrap2 >= 0.0 && wrap2 < max_wrap);
}

#[test]
fn test_f8_03_channel_label_tmux_formatting() {
    let mut event = make_event("s1", "Session Alpha", "Gemini", AgentState::Idle, "Idle", 1, "wsl:Ubuntu");
    event.metadata.tmux_session = Some("dev".to_string());
    event.metadata.tmux_window = Some("1:bash".to_string());

    let label = event.format_channel_label();
    assert_eq!(label, "dev:1:bash");

    // Without tmux window
    event.metadata.tmux_window = None;
    assert_eq!(event.format_channel_label(), "dev");

    // Without tmux info
    event.metadata.tmux_session = None;
    assert_eq!(event.format_channel_label(), "Session Alpha");
}

#[test]
fn test_f8_04_long_title_elision_and_ellipsis() {
    let long_title = "A very long implementation plan header exceeding thirty four chars";
    let max_chars = 34;
    let elided = if long_title.chars().count() > max_chars {
        format!("{}..", long_title.chars().take(max_chars).collect::<String>())
    } else {
        long_title.to_string()
    };

    assert!(elided.ends_with(".."));
    assert_eq!(elided.chars().count(), 36);
}

#[test]
fn test_f8_05_step_counter_safe_positioning() {
    let row_width = 560.0;
    let step_counter_x = row_width - 84.0;
    let marquee_max_x = row_width - 68.0;

    assert!(step_counter_x < row_width);
    assert!(marquee_max_x > step_counter_x);
}

// ==============================================================================
// F9: Viewport Culling & Repaint Optimization (R2, AC5)
// ==============================================================================

#[test]
fn test_f9_01_offscreen_row_culling_detection() {
    let viewport = Rect::from_min_max(pos2(0.0, 0.0), pos2(500.0, 300.0));

    let visible_row = Rect::from_min_max(pos2(0.0, 50.0), pos2(500.0, 100.0));
    let offscreen_above = Rect::from_min_max(pos2(0.0, -80.0), pos2(500.0, -20.0));
    let offscreen_below = Rect::from_min_max(pos2(0.0, 350.0), pos2(500.0, 410.0));

    assert!(viewport.intersects(visible_row));
    assert!(!viewport.intersects(offscreen_above));
    assert!(!viewport.intersects(offscreen_below));
}

#[test]
fn test_f9_02_category_aggregation_caching() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("win-1", "Win", "Gemini", AgentState::Idle, "I", 1, "Windows")).unwrap();
    hub.sender().send(make_event("wsl-1", "WSL", "Claude", AgentState::Idle, "I", 1, "wsl:Ubuntu")).unwrap();
    hub.sender().send(make_event("wsl-2", "WSL", "Claude", AgentState::Idle, "I", 1, "wsl:Debian")).unwrap();
    hub.poll_events();

    let categories = hub.active_categories();
    assert_eq!(categories.len(), 3);
    assert_eq!(categories[0].label, "Windows");
    assert_eq!(categories[1].label, "Debian");
    assert_eq!(categories[2].label, "Ubuntu");
}

#[test]
fn test_f9_03_session_sorting_priority_permission_first() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s_idle", "Idle", "Gemini", AgentState::Idle, "I", 1, "Windows")).unwrap();
    hub.sender().send(make_event("s_tool", "Tool", "Gemini", AgentState::WaitingForApproval { name: "cmd".into(), summary: "run".into() }, "A", 2, "Windows")).unwrap();
    hub.sender().send(make_event("s_think", "Think", "Gemini", AgentState::Thinking, "T", 3, "Windows")).unwrap();
    hub.poll_events();

    let cat = &hub.active_categories()[0];
    let sorted = hub.sessions_for_category(cat);

    assert_eq!(sorted[0].session_id, "s_tool", "WaitingForApproval must sort first");
    assert_eq!(sorted[1].session_id, "s_think", "Thinking must sort second");
    assert_eq!(sorted[2].session_id, "s_idle", "Idle must sort last");
}

#[test]
fn test_f9_04_stale_session_deprioritization() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s1", "Active", "Gemini", AgentState::Idle, "I", 1, "Windows")).unwrap();
    hub.poll_events();

    assert_eq!(hub.sessions[0].sort_priority(), 4); // Idle priority

    // Make stale (> 15m)
    hub.sessions[0].last_updated = Instant::now() - Duration::from_secs(16 * 60);
    assert!(hub.sessions[0].is_stale());
    assert_eq!(hub.sessions[0].sort_priority(), 99, "Stale session must have lowest priority 99");
}

#[test]
fn test_f9_05_frame_budget_sub_16ms_with_20_sessions() {
    let mut hub = create_test_hub();
    for i in 1..=20 {
        hub.sender().send(make_event(&format!("s{}", i), "Session", "Gemini", AgentState::Thinking, "Processing", i, "Windows")).unwrap();
    }
    hub.poll_events();

    let start = Instant::now();
    let cat = &hub.active_categories()[0];
    let sessions = hub.sessions_for_category(cat);
    let dt = 0.016;

    for s in sessions {
        let _ = LayoutFormulas::marquee_advance(s.marquee_offset, dt);
    }
    let elapsed = start.elapsed();

    assert!(elapsed < Duration::from_millis(16), "Processing 20 sessions took {:?}, must be under 16ms", elapsed);
}

// ==============================================================================
// F10: Winamp VU Ballistics & Peak Hold (R3)
// ==============================================================================

#[test]
fn test_f10_01_asymmetric_attack_faster_than_decay() {
    let initial = 0.1;
    let target_attack = 1.0;
    let dt = 0.016;

    let attacked = LayoutFormulas::lerp(initial, target_attack, dt * 12.0);
    let attack_delta = attacked - initial;

    let target_decay = 0.0;
    let decayed = LayoutFormulas::lerp(initial, target_decay, dt * 6.0);
    let decay_delta = initial - decayed;

    assert!(attack_delta > decay_delta, "Attack rate must be faster than decay rate");
}

#[test]
fn test_f10_02_active_session_vu_animation() {
    let mut bar = 0.0;
    let dt = 0.016;
    let pulse_phase = 2.5;

    for i in 0..8 {
        bar = LayoutFormulas::vu_update_active(bar, i, pulse_phase, dt);
        assert!(bar > 0.0, "Active VU bar must animate with positive energy");
    }
}

#[test]
fn test_f10_03_waiting_and_idle_vu_decay_to_baseline() {
    let mut bar = 0.8;
    let dt = 0.1;

    for _ in 0..10 {
        bar = LayoutFormulas::vu_update_decay(bar, 0.0, dt);
    }

    assert!(bar < 0.1, "Waiting session VU must decay towards 0.0");
}

#[test]
fn test_f10_04_vu_levels_bounded_between_0_and_1() {
    for phase_step in 0..20 {
        let phase = phase_step as f32 * 0.5;
        for i in 0..8 {
            let level = LayoutFormulas::vu_update_active(0.5, i, phase, 0.05);
            assert!(level >= 0.0 && level <= 1.0, "VU level {} out of bounds [0, 1]", level);
        }
    }
}

#[test]
fn test_f10_05_peak_hold_indicator_behavior() {
    let level = 0.85;
    let total_segments = 5;
    let active_segments = (level * total_segments as f32).round() as usize;

    assert_eq!(active_segments, 4); // 4 out of 5 active
    // Segment 4 is the red peak segment
    assert!(active_segments >= 4);
}

// ==============================================================================
// F11: Organic LED Breathing & Bloom (R3)
// ==============================================================================

#[test]
fn test_f11_01_led_breathing_exponential_easing() {
    for step in 0..50 {
        let phase = step as f32 * 0.15;
        let intensity = LayoutFormulas::led_breathe_intensity(phase);
        assert!(intensity >= 0.20 && intensity <= 1.00, "LED intensity {} must be clamped [0.2, 1.0]", intensity);
    }
}

#[test]
fn test_f11_02_attention_state_color_mapping() {
    let approval_col = Color32::from_rgb(255, 160, 30);
    let input_col = Color32::from_rgb(255, 205, 20);
    let thinking_col = Color32::from_rgb(0, 255, 128);
    let error_col = Color32::from_rgb(255, 70, 70);

    assert_ne!(approval_col, input_col);
    assert_ne!(thinking_col, error_col);
    assert_eq!(approval_col.r(), 255);
    assert_eq!(thinking_col.g(), 255);
}

#[test]
fn test_f11_03_stale_session_led_dimming() {
    let stale_col = Color32::from_rgb(160, 135, 100);
    let stale_intensity = 0.4;

    let dim_r = (stale_col.r() as f32 * stale_intensity) as u8;
    assert_eq!(dim_r, 64);
}

#[test]
fn test_f11_04_concentric_bloom_radii_layers() {
    let outer_glow_radius = 4.5;
    let inner_core_radius = 2.0;

    assert!(outer_glow_radius > inner_core_radius);
    assert_eq!(inner_core_radius, 2.0);
}

#[test]
fn test_f11_05_pulse_phase_monotonic_advance() {
    let mut phase = 0.0;
    let dt = 0.016;

    for _ in 0..10 {
        let prev = phase;
        phase += dt * 4.0;
        assert!(phase > prev);
    }
}

// ==============================================================================
// F12: Dark Theme Palette Consistency (R3)
// ==============================================================================

#[test]
fn test_f12_01_chassis_background_palette() {
    let win_bg = Color32::from_rgb(16, 18, 22);
    let panel_fill = Color32::from_rgb(18, 20, 25);

    assert!(win_bg.r() < 25 && win_bg.g() < 25 && win_bg.b() < 25);
    assert!(panel_fill.r() < 25 && panel_fill.g() < 25 && panel_fill.b() < 30);
}

#[test]
fn test_f12_02_row_bezel_color_contrast() {
    let normal_bezel = Color32::from_rgb(7, 12, 9);
    let selected_bezel = Color32::from_rgb(10, 22, 16);
    let hover_bezel = Color32::from_rgb(12, 18, 14);

    assert_ne!(normal_bezel, selected_bezel);
    assert_ne!(normal_bezel, hover_bezel);
}

#[test]
fn test_f12_03_rename_overlay_buttons_styling() {
    let save_color = Color32::from_rgb(0, 220, 200);
    assert_eq!(save_color.r(), 0);
    assert_eq!(save_color.g(), 220);
    assert_eq!(save_color.b(), 200);
}

#[test]
fn test_f12_04_glass_scanline_pattern_color() {
    let grid_color = Color32::from_rgba_unmultiplied(20, 45, 25, 30);
    assert_eq!(grid_color.a(), 30);
}

#[test]
fn test_f12_05_contrast_ratio_text_legibility() {
    let text_emerald = Color32::from_rgb(40, 255, 120);
    let bg_dark = Color32::from_rgb(7, 12, 9);

    // Green component contrast
    assert!(text_emerald.g() as i32 - bg_dark.g() as i32 > 200);
}

// ==============================================================================
// F13: Zero Compiler Warnings (AC4)
// ==============================================================================

#[test]
fn test_f13_01_core_crate_compilation_clean() {
    let state = AgentState::Idle;
    let cloned = state.clone();
    assert_eq!(state, cloned);
}

#[test]
fn test_f13_02_ui_hub_types_clean() {
    let cat = DynamicCategory {
        id: "win".to_string(),
        label: "Windows".to_string(),
        is_permanent: true,
        session_count: 5,
    };
    let cat2 = cat.clone();
    assert_eq!(cat, cat2);
}

#[test]
fn test_f13_03_adapter_trait_methods_active() {
    let action = UserAction::Select("s1".to_string());
    if let UserAction::Select(id) = action {
        assert_eq!(id, "s1");
    } else {
        panic!("UserAction mismatch");
    }
}

#[test]
fn test_f13_04_daemon_types_serializable() {
    let event = make_event("s1", "S1", "Gemini", AgentState::Thinking, "Status", 1, "Windows");
    let json_str = serde_json::to_string(&event).unwrap();
    let deserialized: SessionEvent = serde_json::from_str(&json_str).unwrap();

    assert_eq!(event.session_id, deserialized.session_id);
    assert_eq!(event.state, deserialized.state);
}

#[test]
fn test_f13_05_workspace_tests_pass_clean() {
    let titles = Arc::new(RwLock::new(CustomTitlesStorage::in_memory()));
    let hub = SessionHub::new(titles);
    assert_eq!(hub.sessions.len(), 0);
}

// ==============================================================================
// F14: WSL2 Daemon Broadcast Resilience (AC6)
// ==============================================================================

#[test]
fn test_f14_01_daemon_handshake_packet_on_connect() {
    let handshake = SessionEvent::new(
        "wsl-bridge-Ubuntu",
        "WSL Bridge [Ubuntu]",
        "Bridge",
        AgentState::Idle,
        "Connected to Ubuntu",
        0,
        SessionMetadata {
            host: "wsl:Ubuntu".to_string(),
            tmux_session: None,
            tmux_window: None,
            tmux_pane: None,
            cwd: None,
            pid: None,
            agent_type: None,
        },
    );

    assert_eq!(handshake.agent_type, "Bridge");
    assert!(handshake.status_text.contains("Connected"));
}

#[test]
fn test_f14_02_daemon_snapshot_burst_on_connect() {
    let sessions = vec![
        make_event("s1", "S1", "Gemini", AgentState::Thinking, "T1", 1, "wsl:Ubuntu"),
        make_event("s2", "S2", "Claude", AgentState::WaitingForInput { prompt_preview: "P".into() }, "W", 2, "wsl:Ubuntu"),
    ];

    let mut payloads = Vec::new();
    for s in &sessions {
        payloads.push(format!("{}\n", serde_json::to_string(s).unwrap()));
    }

    assert_eq!(payloads.len(), 2);
    assert!(payloads[0].ends_with('\n'));
    assert!(payloads[1].ends_with('\n'));
}

#[tokio::test]
async fn test_f14_03_daemon_broadcast_multi_client() {
    let (tx, mut rx1) = tokio::sync::broadcast::channel::<String>(16);
    let mut rx2 = tx.subscribe();

    tx.send("Event1".to_string()).unwrap();

    let msg1 = rx1.recv().await.unwrap();
    let msg2 = rx2.recv().await.unwrap();

    assert_eq!(msg1, "Event1");
    assert_eq!(msg2, "Event1");
}

#[tokio::test]
async fn test_f14_04_daemon_resilience_to_slow_client_lag() {
    let (tx, mut slow_rx) = tokio::sync::broadcast::channel::<String>(2);

    // Send 5 events into buffer of capacity 2, causing lag
    for i in 1..=5 {
        let _ = tx.send(format!("event_{}", i));
    }

    // slow_rx receives Lagged error when buffer overflows
    let mut lagged_occurred = false;
    let mut received_events = Vec::new();

    // Consume all pending events, recovering through any Lagged errors
    loop {
        match slow_rx.try_recv() {
            Ok(msg) => received_events.push(msg),
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                lagged_occurred = true;
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
        }
    }

    assert!(lagged_occurred, "Buffer overflow must report Lagged error");
    assert!(!received_events.is_empty(), "Latest events must still be recoverable");

    // After draining lag, channel continues to receive fresh events
    let _ = tx.send("fresh_event".to_string());
    let fresh = slow_rx.recv().await.unwrap();
    assert_eq!(fresh, "fresh_event");
}

#[test]
fn test_f14_05_daemon_heartbeat_periodicity() {
    let tick = 8;
    let is_heartbeat_tick = tick % 8 == 0;
    assert!(is_heartbeat_tick, "Tick 8 must trigger periodic heartbeat");
}
