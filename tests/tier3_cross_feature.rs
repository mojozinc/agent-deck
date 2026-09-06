mod common;

use agent_deck_core::AgentState;
use common::{
    append_raw_bytes, create_test_hub, make_event,
    LayoutFormulas, TestTempDir, UserAction,
};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};

// ==============================================================================
// Tier 3: Cross-Feature Combinations (Pairwise Interactions)
// ==============================================================================

#[test]
fn test_t3_01_rapid_dismiss_and_background_poll() {
    let mut hub = create_test_hub();
    for i in 1..=10 {
        hub.sender().send(make_event(&format!("s{}", i), "Session", "Gemini", AgentState::Idle, "I", 1, "Windows")).unwrap();
    }
    hub.poll_events();
    assert_eq!(hub.sessions.len(), 10);

    // Concurrently queue new events while applying dismissals
    hub.sender().send(make_event("s11", "Session 11", "Gemini", AgentState::Thinking, "T", 1, "Windows")).unwrap();
    hub.sender().send(make_event("s12", "Session 12", "Gemini", AgentState::Thinking, "T", 1, "Windows")).unwrap();

    let dismissals = vec![
        UserAction::Dismiss("s1".to_string()),
        UserAction::Dismiss("s2".to_string()),
        UserAction::Dismiss("s3".to_string()),
    ];
    hub.apply_actions(dismissals);
    hub.poll_events();

    assert_eq!(hub.sessions.len(), 9); // 10 - 3 dismissed + 2 new = 9
    assert!(!hub.sessions.iter().any(|s| s.session_id == "s1"));
    assert!(hub.sessions.iter().any(|s| s.session_id == "s11"));
}

#[test]
fn test_t3_02_rename_and_font_scale_change() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s1", "Original", "Gemini", AgentState::Thinking, "T", 1, "Windows")).unwrap();
    hub.poll_events();

    let mut font_scale = 1.0;
    assert_eq!(LayoutFormulas::normal_row_height(font_scale), 52.0);

    // User renames session
    hub.apply_actions(vec![UserAction::Rename("s1".to_string(), "Renamed Fast".to_string())]);
    assert_eq!(hub.sessions[0].display_name, "Renamed Fast");

    // Dynamically zoom in to 1.5x
    font_scale = LayoutFormulas::clamp_font_scale(font_scale + 0.5);
    assert_eq!(font_scale, 1.5);
    let row_h = LayoutFormulas::normal_row_height(font_scale);
    assert!((row_h - (52.0 * 1.30)).abs() < 0.01);
}

#[test]
fn test_t3_03_tool_call_permission_deny_and_abort() {
    let mut hub = create_test_hub();
    // 1. Thinking
    hub.sender().send(make_event("s1", "S1", "Gemini", AgentState::Thinking, "Analyzing", 1, "Windows")).unwrap();
    hub.poll_events();

    // 2. Propose tool call requiring permission
    hub.sender().send(make_event(
        "s1",
        "S1",
        "Gemini",
        AgentState::WaitingForApproval { name: "rm_rf".into(), summary: "Delete dir".into() },
        "PERMISSION REQUIRED",
        2,
        "Windows",
    )).unwrap();
    hub.poll_events();
    assert!(hub.sessions[0].attention.is_unacknowledged);

    // 3. User denies / turn aborts -> WaitingForInput
    hub.sender().send(make_event(
        "s1",
        "S1",
        "Gemini",
        AgentState::WaitingForInput { prompt_preview: "Query aborted by user".into() },
        "ABORTED",
        3,
        "Windows",
    )).unwrap();
    hub.poll_events();
    assert!(matches!(hub.sessions[0].state, AgentState::WaitingForInput { .. }));
}

#[test]
fn test_t3_04_wsl_bridge_connect_and_category_discovery() {
    let mut hub = create_test_hub();
    assert_eq!(hub.active_categories().len(), 1); // Only Windows

    // WSL bridge heartbeat arrives
    let bridge_hb = make_event("wsl-bridge-Ubuntu", "WSL Bridge [Ubuntu]", "Bridge", AgentState::Idle, "Connected", 0, "wsl:Ubuntu");
    hub.sender().send(bridge_hb).unwrap();
    hub.poll_events();

    // WSL session arrives
    let wsl_session = make_event("wsl-1", "Worker", "Claude", AgentState::Thinking, "T", 1, "wsl:Ubuntu");
    hub.sender().send(wsl_session).unwrap();
    hub.poll_events();

    let categories = hub.active_categories();
    assert_eq!(categories.len(), 2);
    assert_eq!(categories[0].label, "Windows");
    assert_eq!(categories[1].label, "Ubuntu");
    assert_eq!(categories[1].session_count, 1);
}

#[test]
fn test_t3_05_alert_pulsing_and_category_acknowledge() {
    let mut hub = create_test_hub();
    // Start both
    hub.sender().send(make_event("win-1", "Win", "Gemini", AgentState::Thinking, "T", 1, "Windows")).unwrap();
    hub.sender().send(make_event("wsl-1", "WSL", "Claude", AgentState::Thinking, "T", 1, "wsl:Ubuntu")).unwrap();
    hub.poll_events();

    // Both enter waiting for approval
    hub.sender().send(make_event("win-1", "Win", "Gemini", AgentState::WaitingForApproval { name: "t".into(), summary: "s".into() }, "A", 2, "Windows")).unwrap();
    hub.sender().send(make_event("wsl-1", "WSL", "Claude", AgentState::WaitingForApproval { name: "t".into(), summary: "s".into() }, "A", 2, "wsl:Ubuntu")).unwrap();
    hub.poll_events();

    assert!(hub.sessions[0].attention.is_unacknowledged);
    assert!(hub.sessions[1].attention.is_unacknowledged);

    // Acknowledge WSL category only
    hub.apply_actions(vec![UserAction::AcknowledgeCategory("host:Ubuntu".to_string())]);

    let win_session = hub.sessions.iter().find(|s| s.session_id == "win-1").unwrap();
    let wsl_session = hub.sessions.iter().find(|s| s.session_id == "wsl-1").unwrap();

    assert!(win_session.attention.is_unacknowledged, "Windows alert must remain unacknowledged");
    assert!(!wsl_session.attention.is_unacknowledged, "WSL alert must be cleared");
}

#[test]
fn test_t3_06_marquee_looping_under_dynamic_rescaling() {
    let text = "Running extensive test suite across multiple agent terminals";
    let mut offset = 0.0;
    let dt = 0.016;

    for step in 0..100 {
        let scale = 0.85 + (step % 15) as f32 * 0.05;
        offset = LayoutFormulas::marquee_advance(offset, dt);
        let wrapped = LayoutFormulas::marquee_modulo_offset(offset, text.len(), scale);
        let max_wrap = (text.len() + 6) as f32 * 7.0 * scale + 40.0;
        assert!(wrapped >= 0.0 && wrapped < max_wrap);
    }
}

#[test]
fn test_t3_07_vu_ballistics_during_state_transition() {
    let mut bar = 0.0;
    let dt = 0.016;

    // Phase 1: Thinking (active wave excitation)
    for step in 0..20 {
        let phase = step as f32 * 0.1;
        bar = LayoutFormulas::vu_update_active(bar, 0, phase, dt);
    }
    assert!(bar > 0.05);

    // Phase 2: Transition to WaitingForInput (decay to 0.0)
    for _ in 0..30 {
        bar = LayoutFormulas::vu_update_decay(bar, 0.0, dt);
    }
    assert!(bar < 0.05, "VU must decay towards zero after entering waiting state");
}

#[test]
fn test_t3_08_stale_detection_and_dismiss_pill_click() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s1", "StaleCandidate", "Gemini", AgentState::Idle, "I", 1, "Windows")).unwrap();
    hub.poll_events();

    assert!(!hub.sessions[0].is_stale());

    // Advance last updated by 16 minutes
    hub.sessions[0].last_updated = Instant::now() - Duration::from_secs(16 * 60);
    assert!(hub.sessions[0].is_stale());

    // Stale session prompts user to click [DISMISS] pill
    hub.apply_actions(vec![UserAction::Dismiss("s1".to_string())]);
    assert_eq!(hub.sessions.len(), 0);
    assert!(hub.dismissed_sessions.contains("s1"));
}

#[test]
fn test_t3_09_sorting_reorder_on_state_change() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s1", "Session 1", "Gemini", AgentState::Idle, "I", 1, "Windows")).unwrap();
    hub.sender().send(make_event("s2", "Session 2", "Gemini", AgentState::Thinking, "T", 1, "Windows")).unwrap();
    hub.poll_events();

    let cat = &hub.active_categories()[0];
    let initial_sort = hub.sessions_for_category(cat);
    assert_eq!(initial_sort[0].session_id, "s2", "Thinking sorts before Idle");

    // Session 1 receives Permission Request (Priority 1)
    hub.sender().send(make_event("s1", "Session 1", "Gemini", AgentState::WaitingForApproval { name: "t".into(), summary: "s".into() }, "A", 2, "Windows")).unwrap();
    hub.poll_events();

    let updated_sort = hub.sessions_for_category(cat);
    assert_eq!(updated_sort[0].session_id, "s1", "WaitingForApproval must jump to position 1");
}

#[test]
fn test_t3_10_claude_transcript_ingestion_and_custom_rename() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("claude-uuid", "claude-uuid", "Claude", AgentState::Thinking, "Thinking", 1, "wsl:Ubuntu")).unwrap();
    hub.poll_events();
    assert_eq!(hub.sessions[0].display_name, "claude-uuid");

    // Rename to friendly name
    hub.apply_actions(vec![UserAction::Rename("claude-uuid".to_string(), "Backend API Refactor".to_string())]);
    assert_eq!(hub.sessions[0].display_name, "Backend API Refactor");

    // New event comes in from Claude parser
    hub.sender().send(make_event("claude-uuid", "claude-uuid", "Claude", AgentState::Thinking, "Executing tool", 2, "wsl:Ubuntu")).unwrap();
    hub.poll_events();

    assert_eq!(hub.sessions[0].display_name, "Backend API Refactor", "Custom title must be preserved across events");
}

#[test]
fn test_t3_11_partial_line_write_with_concurrent_poll() {
    let temp = TestTempDir::new("t3_11");
    let path = temp.file_path("poll.jsonl");

    // Step 1: Writer emits partial line
    append_raw_bytes(&path, b"{\"step_index\": 1, \"content\": \"incom");

    // Step 2: Reader tries to parse lines
    {
        let file = File::open(&path).unwrap();
        let lines: Vec<String> = BufReader::new(file).lines().flatten().collect();
        assert_eq!(lines.len(), 1);
        assert!(serde_json::from_str::<serde_json::Value>(&lines[0]).is_err());
    }

    // Step 3: Writer finishes line
    append_raw_bytes(&path, b"plete\"}\n");

    // Step 4: Next poll consumes full event cleanly
    {
        let file = File::open(&path).unwrap();
        let lines: Vec<String> = BufReader::new(file).lines().flatten().collect();
        assert_eq!(lines.len(), 1);
        let val: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(val["step_index"], 1);
    }
}

#[test]
fn test_t3_12_multi_distro_wsl_switch_and_filter() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("u1", "Ubuntu 1", "Gemini", AgentState::Idle, "I", 1, "wsl:Ubuntu")).unwrap();
    hub.sender().send(make_event("u2", "Ubuntu 2", "Gemini", AgentState::Idle, "I", 1, "wsl:Ubuntu")).unwrap();
    hub.sender().send(make_event("d1", "Debian 1", "Gemini", AgentState::Idle, "I", 1, "wsl:Debian")).unwrap();
    hub.poll_events();

    let categories = hub.active_categories();
    let debian_cat = categories.iter().find(|c| c.label == "Debian").unwrap();
    let ubuntu_cat = categories.iter().find(|c| c.label == "Ubuntu").unwrap();

    let debian_sessions = hub.sessions_for_category(debian_cat);
    let ubuntu_sessions = hub.sessions_for_category(ubuntu_cat);

    assert_eq!(debian_sessions.len(), 1);
    assert_eq!(ubuntu_sessions.len(), 2);
}

#[test]
fn test_t3_13_inline_rename_save_cancel_flow() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s_rename_flow_13", "Original Name", "Gemini", agent_deck_core::AgentState::Idle, "I", 1, "Windows")).unwrap();
    hub.poll_events();

    // User types edit buffer and cancels: no action dispatched
    assert_eq!(hub.sessions[0].display_name, "Original Name");

    // User types edit buffer and clicks Save
    hub.apply_actions(vec![UserAction::Rename("s_rename_flow_13".to_string(), "New Saved Name".to_string())]);
    assert_eq!(hub.sessions[0].display_name, "New Saved Name");

    // User clicks Reset: empty rename dispatched
    hub.apply_actions(vec![UserAction::Rename("s_rename_flow_13".to_string(), "".to_string())]);
    assert_eq!(hub.custom_titles.read().unwrap().get_title("s_rename_flow_13"), None);
}

#[test]
fn test_t3_14_high_load_25_sessions_with_frequent_dismissals() {
    let mut hub = create_test_hub();
    for i in 1..=25 {
        hub.sender().send(make_event(&format!("s{}", i), "Session", "Gemini", AgentState::Thinking, "T", i, "Windows")).unwrap();
    }
    hub.poll_events();
    assert_eq!(hub.sessions.len(), 25);

    // Dismiss 10 sessions in burst
    let dismissals: Vec<UserAction> = (1..=10).map(|i| UserAction::Dismiss(format!("s{}", i))).collect();
    hub.apply_actions(dismissals);

    assert_eq!(hub.sessions.len(), 15);
    for i in 1..=10 {
        assert!(!hub.sessions.iter().any(|s| s.session_id == format!("s{}", i)));
    }
}

#[test]
fn test_t3_15_resurrect_dismissed_session_with_attention_alert() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s1", "S1", "Gemini", AgentState::Thinking, "T", 10, "Windows")).unwrap();
    hub.poll_events();

    hub.apply_actions(vec![UserAction::Dismiss("s1".to_string())]);
    assert_eq!(hub.sessions.len(), 0);

    // New active turn arrives at step 11 requesting permission
    hub.sender().send(make_event(
        "s1",
        "S1",
        "Gemini",
        AgentState::WaitingForApproval { name: "deploy".into(), summary: "deploy to prod".into() },
        "PERMISSION REQUIRED",
        11,
        "Windows",
    )).unwrap();
    hub.poll_events();

    assert_eq!(hub.sessions.len(), 1, "Dismissed session must resurrect on new turn");
    assert_eq!(hub.sessions[0].step_count, 11);
    assert!(matches!(hub.sessions[0].state, AgentState::WaitingForApproval { .. }));
}
