mod common;

use agent_deck_core::{AgentState, SessionEvent, SessionMetadata};
use common::{create_test_hub, make_event, LayoutFormulas, UserAction};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as TokioBufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;

// ==============================================================================
// Tier 4: Real-World Application Scenarios
// ==============================================================================

#[test]
fn test_t4_01_full_antigravity_multi_turn_stream() {
    let mut hub = create_test_hub();
    let session_id = "t4_agy_session_1";

    // Turn 1: User prompt
    hub.sender().send(make_event(
        session_id,
        "Antigravity Agent",
        "Gemini",
        AgentState::Thinking,
        "PROCESSING: Fix auth token validation bug",
        1,
        "Windows",
    )).unwrap();
    hub.poll_events();
    assert_eq!(hub.sessions[0].state, AgentState::Thinking);
    assert_eq!(hub.sessions[0].step_count, 1);

    // Turn 2: Tool proposed -> WaitingForApproval
    hub.sender().send(make_event(
        session_id,
        "Antigravity Agent",
        "Gemini",
        AgentState::WaitingForApproval {
            name: "grep_search".to_string(),
            summary: "Searching for verify_token".to_string(),
        },
        "PERMISSION REQUIRED: grep_search (Searching for verify_token)",
        2,
        "Windows",
    )).unwrap();
    hub.poll_events();
    assert!(matches!(hub.sessions[0].state, AgentState::WaitingForApproval { .. }));
    assert!(hub.sessions[0].attention.is_unacknowledged);

    // Turn 3: User approves, tool executes -> RunningTool
    hub.sender().send(make_event(
        session_id,
        "Antigravity Agent",
        "Gemini",
        AgentState::RunningTool {
            name: "grep_search".to_string(),
            summary: "Searching for verify_token".to_string(),
        },
        "TOOL grep_search: Searching for verify_token",
        3,
        "Windows",
    )).unwrap();
    hub.poll_events();
    assert!(matches!(hub.sessions[0].state, AgentState::RunningTool { .. }));
    assert!(!hub.sessions[0].attention.is_unacknowledged);

    // Turn 4: Edit tool proposed -> WaitingForApproval
    hub.sender().send(make_event(
        session_id,
        "Antigravity Agent",
        "Gemini",
        AgentState::WaitingForApproval {
            name: "replace_file_content".to_string(),
            summary: "Patch token expiry check".to_string(),
        },
        "PERMISSION REQUIRED: replace_file_content",
        4,
        "Windows",
    )).unwrap();
    hub.poll_events();
    assert!(matches!(hub.sessions[0].state, AgentState::WaitingForApproval { .. }));

    // Turn 5: Edit tool executes -> RunningTool
    hub.sender().send(make_event(
        session_id,
        "Antigravity Agent",
        "Gemini",
        AgentState::RunningTool {
            name: "replace_file_content".to_string(),
            summary: "Patch token expiry check".to_string(),
        },
        "TOOL replace_file_content: Patch token expiry check",
        5,
        "Windows",
    )).unwrap();
    hub.poll_events();
    assert!(matches!(hub.sessions[0].state, AgentState::RunningTool { .. }));

    // Turn 6: Turn finishes -> WaitingForInput
    hub.sender().send(make_event(
        session_id,
        "Antigravity Agent",
        "Gemini",
        AgentState::WaitingForInput {
            prompt_preview: "Patched token verification in session.rs".to_string(),
        },
        "WAITING FOR PROMPT",
        6,
        "Windows",
    )).unwrap();
    hub.poll_events();
    assert!(matches!(hub.sessions[0].state, AgentState::WaitingForInput { .. }));
    assert_eq!(hub.sessions[0].step_count, 6);
}

#[test]
fn test_t4_02_claude_code_multi_tool_cascade() {
    let mut hub = create_test_hub();
    let session_id = "t4_claude_cascade_2";

    // 1. Initial user prompt
    hub.sender().send(make_event(
        session_id,
        "Claude Code Agent",
        "Claude",
        AgentState::Thinking,
        "PROCESSING: Run full test audit and patch failures",
        1,
        "wsl:Ubuntu",
    )).unwrap();
    hub.poll_events();

    // 2. Cascade Tool 1: Bash (cargo test)
    hub.sender().send(make_event(
        session_id,
        "Claude Code Agent",
        "Claude",
        AgentState::RunningTool { name: "Bash".into(), summary: "cargo test --workspace".into() },
        "TOOL Bash: cargo test --workspace",
        2,
        "wsl:Ubuntu",
    )).unwrap();
    hub.poll_events();
    assert_eq!(hub.sessions[0].step_count, 2);

    // 3. Cascade Tool 2: Grep (find failure lines)
    hub.sender().send(make_event(
        session_id,
        "Claude Code Agent",
        "Claude",
        AgentState::RunningTool { name: "Grep".into(), summary: "pattern: FAILED".into() },
        "TOOL Grep: pattern: FAILED",
        3,
        "wsl:Ubuntu",
    )).unwrap();
    hub.poll_events();

    // 4. Cascade Tool 3: Edit (patch test)
    hub.sender().send(make_event(
        session_id,
        "Claude Code Agent",
        "Claude",
        AgentState::RunningTool { name: "Edit".into(), summary: "Patching line 42".into() },
        "TOOL Edit: Patching line 42",
        4,
        "wsl:Ubuntu",
    )).unwrap();
    hub.poll_events();

    // 5. Turn completion -> WaitingForInput
    hub.sender().send(make_event(
        session_id,
        "Claude Code Agent",
        "Claude",
        AgentState::WaitingForInput { prompt_preview: "All 162 test cases passing".into() },
        "WAITING FOR PROMPT",
        5,
        "wsl:Ubuntu",
    )).unwrap();
    hub.poll_events();

    assert_eq!(hub.sessions[0].step_count, 5);
    assert!(matches!(hub.sessions[0].state, AgentState::WaitingForInput { .. }));
}

#[test]
fn test_t4_03_concurrent_multi_environment_workload() {
    let mut hub = create_test_hub();

    // Windows Native sessions
    hub.sender().send(make_event("win-1", "Win Agent 1", "Gemini", AgentState::Thinking, "T", 1, "Windows")).unwrap();
    hub.sender().send(make_event("win-2", "Win Agent 2", "Gemini", AgentState::Idle, "I", 1, "Windows")).unwrap();

    // WSL Ubuntu sessions
    hub.sender().send(make_event("wsl-u1", "Ubuntu 1", "Claude", AgentState::Thinking, "T", 1, "wsl:Ubuntu")).unwrap();
    hub.sender().send(make_event("wsl-u2", "Ubuntu 2", "Claude", AgentState::Idle, "I", 1, "wsl:Ubuntu")).unwrap();
    hub.sender().send(make_event("wsl-u3", "Ubuntu 3", "Claude", AgentState::WaitingForInput { prompt_preview: "P".into() }, "W", 1, "wsl:Ubuntu")).unwrap();

    // WSL Debian sessions
    hub.sender().send(make_event("wsl-d1", "Debian 1", "Gemini", AgentState::Thinking, "T", 1, "wsl:Debian")).unwrap();
    hub.sender().send(make_event("wsl-d2", "Debian 2", "Gemini", AgentState::Idle, "I", 1, "wsl:Debian")).unwrap();

    hub.poll_events();

    let categories = hub.active_categories();
    assert_eq!(categories.len(), 3); // Windows, Debian, Ubuntu

    let win_cat = categories.iter().find(|c| c.label == "Windows").unwrap();
    let ubu_cat = categories.iter().find(|c| c.label == "Ubuntu").unwrap();
    let deb_cat = categories.iter().find(|c| c.label == "Debian").unwrap();

    assert_eq!(win_cat.session_count, 2);
    assert_eq!(ubu_cat.session_count, 3);
    assert_eq!(deb_cat.session_count, 2);

    assert_eq!(hub.sessions_for_category(win_cat).len(), 2);
    assert_eq!(hub.sessions_for_category(ubu_cat).len(), 3);
    assert_eq!(hub.sessions_for_category(deb_cat).len(), 2);
}

#[test]
fn test_t4_04_abort_and_recovery_flow() {
    let mut hub = create_test_hub();
    let session_id = "t4_abort_flow_4";

    // 1. Agent starts executing tool
    hub.sender().send(make_event(
        session_id,
        "Agent",
        "Gemini",
        AgentState::Thinking,
        "Processing large codebase",
        1,
        "Windows",
    )).unwrap();
    hub.poll_events();

    hub.sender().send(make_event(
        session_id,
        "Agent",
        "Gemini",
        AgentState::RunningTool { name: "compile".into(), summary: "Building huge target".into() },
        "RUNNING compile",
        2,
        "Windows",
    )).unwrap();
    hub.poll_events();
    assert!(matches!(hub.sessions[0].state, AgentState::RunningTool { .. }));

    // 2. User presses Ctrl+C / sends abort
    hub.sender().send(make_event(
        session_id,
        "Agent",
        "Gemini",
        AgentState::WaitingForInput { prompt_preview: "Query interrupted by user".into() },
        "ABORTED",
        3,
        "Windows",
    )).unwrap();
    hub.poll_events();

    // 3. System recovers cleanly to WaitingForInput
    assert_eq!(hub.sessions.len(), 1);
    assert!(matches!(hub.sessions[0].state, AgentState::WaitingForInput { .. }));
    assert_eq!(hub.sessions[0].step_count, 3);
    assert_eq!(hub.sessions[0].status_text, "ABORTED");
}

#[test]
fn test_t4_05_high_density_25_session_stress_test() {
    let mut hub = create_test_hub();

    for i in 1..=25 {
        let state = match i % 5 {
            0 => AgentState::WaitingForApproval { name: format!("tool_{}", i), summary: "Perm".into() },
            1 => AgentState::Thinking,
            2 => AgentState::RunningTool { name: format!("tool_{}", i), summary: "Exec".into() },
            3 => AgentState::WaitingForInput { prompt_preview: "Input".into() },
            _ => AgentState::Idle,
        };

        hub.sender().send(make_event(
            &format!("stress_session_{}", i),
            &format!("Session {:02}", i),
            "Gemini",
            state,
            &format!("Status line for session {}", i),
            i as u32,
            if i % 2 == 0 { "Windows" } else { "wsl:Ubuntu" },
        )).unwrap();
    }

    let start = Instant::now();
    hub.poll_events();

    let categories = hub.active_categories();
    for cat in &categories {
        let sessions = hub.sessions_for_category(cat);
        for s in sessions {
            let _ = s.sort_priority();
            let _ = LayoutFormulas::marquee_advance(s.marquee_offset, 0.016);
        }
    }
    let duration = start.elapsed();

    assert_eq!(hub.sessions.len(), 25);
    // Budget test: must execute well under 16ms (typically < 1ms)
    assert!(duration < Duration::from_millis(16), "25 session frame loop took {:?}, must be under 16ms", duration);

    // Verify top prioritized session is indeed WaitingForApproval
    let win_cat = categories.iter().find(|c| c.label == "Windows").unwrap();
    let sorted_win = hub.sessions_for_category(win_cat);
    assert_eq!(sorted_win[0].sort_priority(), 1, "Priority 1 (Approval) must be sorted first");
}

#[tokio::test]
async fn test_t4_06_wsl_daemon_tcp_lifecycle_disconnect_reconnect() {
    // 1. Bind ephemeral TCP port
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = listener.local_addr().unwrap();

    let (tx, _rx) = broadcast::channel::<String>(32);
    let tx_broadcast = tx.clone();

    // 2. Spawn Mock Daemon Server
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let mut rx = tx_broadcast.subscribe();
            let (_reader, mut writer) = socket.into_split();

            // Handshake
            let handshake = SessionEvent::new(
                "wsl-bridge-Test",
                "WSL Bridge [Test]",
                "Bridge",
                AgentState::Idle,
                "Connected",
                0,
                SessionMetadata {
                    host: "wsl:Test".to_string(),
                    tmux_session: None,
                    tmux_window: None,
                    tmux_pane: None,
                    cwd: None,
                    pid: None,
                    agent_type: None,
                },
            );
            let payload = format!("{}\n", serde_json::to_string(&handshake).unwrap());
            let _ = writer.write_all(payload.as_bytes()).await;

            tokio::spawn(async move {
                while let Ok(msg) = rx.recv().await {
                    let p = format!("{}\n", msg);
                    if writer.write_all(p.as_bytes()).await.is_err() {
                        break;
                    }
                }
            });
        }
    });

    // 3. Client 1 Connects
    {
        let stream1 = TcpStream::connect(server_addr).await.unwrap();
        let mut reader1 = TokioBufReader::new(stream1);
        let mut handshake_line = String::new();
        reader1.read_line(&mut handshake_line).await.unwrap();

        let event: SessionEvent = serde_json::from_str(handshake_line.trim()).unwrap();
        assert_eq!(event.agent_type, "Bridge");

        // Broadcast a live event
        let live_event = make_event("wsl-live-1", "Live", "Claude", AgentState::Thinking, "T", 1, "wsl:Test");
        tx.send(serde_json::to_string(&live_event).unwrap()).unwrap();

        let mut live_line = String::new();
        reader1.read_line(&mut live_line).await.unwrap();
        let received_live: SessionEvent = serde_json::from_str(live_line.trim()).unwrap();
        assert_eq!(received_live.session_id, "wsl-live-1");
        // stream1 dropped here (client disconnect)
    }

    // 4. Client 2 Connects (Reconnect lifecycle)
    {
        let stream2 = TcpStream::connect(server_addr).await.unwrap();
        let mut reader2 = TokioBufReader::new(stream2);
        let mut handshake_line2 = String::new();
        reader2.read_line(&mut handshake_line2).await.unwrap();

        let event2: SessionEvent = serde_json::from_str(handshake_line2.trim()).unwrap();
        assert_eq!(event2.agent_type, "Bridge");
    }
}

#[test]
fn test_t4_07_end_to_end_user_action_queue_two_pass() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s1", "Alpha", "Gemini", AgentState::Thinking, "T", 1, "Windows")).unwrap();
    hub.sender().send(make_event("s2", "Beta", "Gemini", AgentState::Idle, "I", 1, "Windows")).unwrap();
    hub.sender().send(make_event("s3", "Gamma", "Gemini", AgentState::Idle, "I", 1, "Windows")).unwrap();
    hub.poll_events();

    assert_eq!(hub.sessions.len(), 3);

    // Pass 1: Render pass collects user actions
    let mut frame_actions = Vec::new();
    // User selects s1
    frame_actions.push(UserAction::Select("s1".to_string()));
    // User dismisses s2
    frame_actions.push(UserAction::Dismiss("s2".to_string()));
    // User renames s3
    frame_actions.push(UserAction::Rename("s3".to_string(), "Gamma Prime".to_string()));

    // Pass 2: Actions applied outside render loops
    hub.apply_actions(frame_actions);

    // Verify all actions took effect cleanly
    assert_eq!(hub.sessions.len(), 2, "Session s2 must be dismissed");
    assert!(!hub.sessions.iter().any(|s| s.session_id == "s2"));

    let s3 = hub.sessions.iter().find(|s| s.session_id == "s3").unwrap();
    assert_eq!(s3.display_name, "Gamma Prime");
}
