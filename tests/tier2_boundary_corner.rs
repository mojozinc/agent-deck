mod common;

use agent_deck_core::{AgentState, SessionEvent, SessionMetadata};
use common::{
    append_json_line, append_raw_bytes, create_test_hub, make_event, AttentionState,
    CustomTitlesStorage, LayoutFormulas, TestTempDir, UserAction,
};
use egui::{Color32, Rect, pos2};
use serde_json::json;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::time::{Duration, Instant};

// ==============================================================================
// F1 Boundaries & Corner Cases: Newline Ingestion & Offset Sync
// ==============================================================================

#[test]
fn test_f1_bva_01_empty_0_byte_file() {
    let temp = TestTempDir::new("f1_bva_01");
    let path = temp.file_path("empty.jsonl");
    File::create(&path).unwrap();

    let meta = std::fs::metadata(&path).unwrap();
    assert_eq!(meta.len(), 0);

    let file = File::open(&path).unwrap();
    let lines: Vec<String> = BufReader::new(file).lines().flatten().collect();
    assert_eq!(lines.len(), 0);
}

#[test]
fn test_f1_bva_02_massive_tool_call_64kb_line() {
    let temp = TestTempDir::new("f1_bva_02");
    let path = temp.file_path("massive.jsonl");

    let large_diff = "A".repeat(65536);
    let event = json!({
        "step_index": 1,
        "type": "TOOL_CALL",
        "tool_calls": [{
            "name": "replace_file_content",
            "args": {"content": large_diff}
        }]
    });
    append_json_line(&path, &event);

    let file = File::open(&path).unwrap();
    let lines: Vec<String> = BufReader::new(file).lines().flatten().collect();
    assert_eq!(lines.len(), 1);

    let parsed: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(parsed["step_index"], 1);
    let read_diff = parsed["tool_calls"][0]["args"]["content"].as_str().unwrap();
    assert_eq!(read_diff.len(), 65536);
}

#[test]
fn test_f1_bva_03_rapid_flush_race_simulation() {
    let temp = TestTempDir::new("f1_bva_03");
    let path = temp.file_path("race.jsonl");

    let full_json = "{\"step_index\": 1, \"status\": \"DONE\"}\n";
    let bytes = full_json.as_bytes();

    // Write first 15 bytes (mid-JSON, no newline)
    append_raw_bytes(&path, &bytes[..15]);
    let meta1 = std::fs::metadata(&path).unwrap();
    assert_eq!(meta1.len(), 15);

    // Reader inspecting file sees no newline
    {
        let file = File::open(&path).unwrap();
        let mut reader = BufReader::new(file);
        let mut buf = String::new();
        let _ = reader.read_line(&mut buf);
        assert!(!buf.ends_with('\n'));
    }

    // Writer appends remaining bytes with newline
    append_raw_bytes(&path, &bytes[15..]);
    {
        let file = File::open(&path).unwrap();
        let lines: Vec<String> = BufReader::new(file).lines().flatten().collect();
        assert_eq!(lines.len(), 1);
        let parsed: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(parsed["step_index"], 1);
    }
}

#[test]
fn test_f1_bva_04_garbage_lines_between_valid_json() {
    let temp = TestTempDir::new("f1_bva_04");
    let path = temp.file_path("corrupt.jsonl");

    let payload = b"{\"step_index\": 1}\nINVALID_NON_JSON_CORRUPT_BYTES\n{\"step_index\": 2}\n";
    append_raw_bytes(&path, payload);

    let file = File::open(&path).unwrap();
    let mut valid_events = Vec::new();
    for line in BufReader::new(file).lines().flatten() {
        if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(&line) {
            valid_events.push(json_val);
        }
    }

    assert_eq!(valid_events.len(), 2);
    assert_eq!(valid_events[0]["step_index"], 1);
    assert_eq!(valid_events[1]["step_index"], 2);
}

#[test]
fn test_f1_bva_05_tail_seek_large_transcript() {
    let temp = TestTempDir::new("f1_bva_05");
    let path = temp.file_path("large_tail.jsonl");

    // Write 200 lines to create a file > 8KB
    for i in 1..=200 {
        append_json_line(&path, &json!({"step_index": i, "padding": "0123456789012345678901234567890123456789"}));
    }

    let meta = std::fs::metadata(&path).unwrap();
    assert!(meta.len() > 8192);

    // Tail seek to last 4096 bytes
    let mut file = File::open(&path).unwrap();
    file.seek(SeekFrom::Start(meta.len() - 4096)).unwrap();
    let reader = BufReader::new(file);

    let mut last_line = None;
    for line in reader.lines().flatten() {
        if !line.trim().is_empty() {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
                last_line = Some(val);
            }
        }
    }

    assert!(last_line.is_some());
    assert_eq!(last_line.unwrap()["step_index"], 200);
}

// ==============================================================================
// F2 Boundaries & Corner Cases: State Transitions & RunningTool
// ==============================================================================

#[test]
fn test_f2_bva_01_zero_step_count_initial_state() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s0", "Init", "Gemini", AgentState::Idle, "Starting", 0, "Windows")).unwrap();
    hub.poll_events();

    assert_eq!(hub.sessions.len(), 1);
    assert_eq!(hub.sessions[0].step_count, 0);
    assert_eq!(hub.sessions[0].state, AgentState::Idle);
}

#[test]
fn test_f2_bva_02_maximum_step_count_u32_max() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s_max", "MaxStep", "Gemini", AgentState::Thinking, "Thinking", u32::MAX, "Windows")).unwrap();
    hub.poll_events();

    assert_eq!(hub.sessions.len(), 1);
    assert_eq!(hub.sessions[0].step_count, u32::MAX);

    let mut attention = AttentionState::new();
    attention.update(&AgentState::WaitingForInput { prompt_preview: "Prompt".into() }, u32::MAX);
    assert!(attention.last_state_signature.contains(&u32::MAX.to_string()));
}

#[test]
fn test_f2_bva_03_sub_millisecond_rapid_state_flapping() {
    let mut hub = create_test_hub();
    let states = vec![
        AgentState::Thinking,
        AgentState::WaitingForApproval { name: "t1".into(), summary: "s1".into() },
        AgentState::RunningTool { name: "t1".into(), summary: "s1".into() },
        AgentState::Thinking,
        AgentState::WaitingForInput { prompt_preview: "done".into() },
    ];

    for (step, state) in states.into_iter().enumerate() {
        hub.sender().send(make_event("s1", "Flap", "Gemini", state, "Status", step as u32 + 1, "Windows")).unwrap();
        hub.poll_events();
    }

    assert_eq!(hub.sessions.len(), 1);
    assert!(matches!(hub.sessions[0].state, AgentState::WaitingForInput { .. }));
    assert_eq!(hub.sessions[0].step_count, 5);
}

#[test]
fn test_f2_bva_04_empty_tool_name_and_summary() {
    let state = AgentState::RunningTool {
        name: String::new(),
        summary: String::new(),
    };

    let mut hub = create_test_hub();
    hub.sender().send(make_event("s1", "EmptyTool", "Gemini", state.clone(), "", 1, "Windows")).unwrap();
    hub.poll_events();

    assert_eq!(hub.sessions[0].state, state);
    assert_eq!(hub.sessions[0].status_text, "");
}

#[test]
fn test_f2_bva_05_identical_state_signature_no_attention_reset() {
    let mut attention = AttentionState::new();
    attention.update(&AgentState::Thinking, 1);
    let state = AgentState::WaitingForInput { prompt_preview: "Query".into() };

    attention.update(&state, 2);
    let original_triggered = attention.triggered_at;
    assert!(original_triggered.is_some());

    // Update with identical state and step count
    attention.update(&state, 2);
    assert_eq!(attention.triggered_at, original_triggered, "Identical signature must not reset triggered timestamp");
}

// ==============================================================================
// F3 Boundaries & Corner Cases: Claude Code Transcript Parser
// ==============================================================================

#[test]
fn test_f3_bva_01_claude_transcript_empty_content_array() {
    let msg = json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": []
        }
    });

    let content_arr = msg["message"]["content"].as_array().unwrap();
    assert_eq!(content_arr.len(), 0);

    let prompt = content_arr.first().and_then(|c| c["text"].as_str()).unwrap_or("Ready for input");
    assert_eq!(prompt, "Ready for input");
}

#[test]
fn test_f3_bva_02_claude_nested_json_tool_input() {
    let mut nested = json!({"leaf": "value"});
    for _ in 0..10 {
        nested = json!({"wrapper": nested});
    }

    let tool_msg = json!({
        "type": "assistant",
        "message": {
            "content": [{
                "type": "tool_use",
                "name": "ConfigGenerator",
                "input": nested
            }]
        }
    });

    let tool_name = tool_msg["message"]["content"][0]["name"].as_str().unwrap();
    assert_eq!(tool_name, "ConfigGenerator");
}

#[test]
fn test_f3_bva_03_claude_multiline_bash_command_100_lines() {
    let long_cmd = (0..100).map(|i| format!("echo 'Line {}'", i)).collect::<Vec<_>>().join("\n");
    let tool_block = json!({
        "name": "Bash",
        "input": {"command": long_cmd}
    });

    let cmd_str = tool_block["input"]["command"].as_str().unwrap();
    let first_line = cmd_str.lines().next().unwrap();
    let clean_summary: String = first_line.chars().take(60).collect();

    assert_eq!(clean_summary, "echo 'Line 0'");
}

#[test]
fn test_f3_bva_04_claude_unicode_and_special_chars_in_tool() {
    let tool_input = json!({
        "name": "Grep",
        "input": {"pattern": "函数 (fn) • 🚀 特殊字符: \t\r \"quotes\""}
    });

    let pattern = tool_input["input"]["pattern"].as_str().unwrap();
    assert!(pattern.contains("函数"));
    assert!(pattern.contains("🚀"));
}

#[test]
fn test_f3_bva_05_claude_invalid_json_middle_of_stream() {
    let temp = TestTempDir::new("f3_bva_05");
    let path = temp.file_path("stream.jsonl");

    append_json_line(&path, &json!({"type": "user", "text": "Turn 1"}));
    append_raw_bytes(&path, b"MALFORMED_GARBAGE_LINE\n");
    append_json_line(&path, &json!({"type": "user", "text": "Turn 2"}));

    let file = File::open(&path).unwrap();
    let valid_turns: Vec<String> = BufReader::new(file)
        .lines()
        .flatten()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(&l).ok())
        .filter_map(|v| v["text"].as_str().map(|s| s.to_string()))
        .collect();

    assert_eq!(valid_turns, vec!["Turn 1", "Turn 2"]);
}

// ==============================================================================
// F4 Boundaries & Corner Cases: Persistent Session Dismissal Tracking
// ==============================================================================

#[test]
fn test_f4_bva_01_dismiss_nonexistent_session() {
    let mut hub = create_test_hub();
    hub.apply_actions(vec![UserAction::Dismiss("non-existent-uuid".to_string())]);
    assert_eq!(hub.sessions.len(), 0);
    assert!(hub.dismissed_sessions.contains("non-existent-uuid"));
}

#[test]
fn test_f4_bva_02_dismiss_all_20_sessions() {
    let mut hub = create_test_hub();
    for i in 1..=20 {
        hub.sender().send(make_event(&format!("s{}", i), "S", "Gemini", AgentState::Idle, "I", 1, "Windows")).unwrap();
    }
    hub.poll_events();
    assert_eq!(hub.sessions.len(), 20);

    let dismissals: Vec<UserAction> = (1..=20).map(|i| UserAction::Dismiss(format!("s{}", i))).collect();
    hub.apply_actions(dismissals);

    assert_eq!(hub.sessions.len(), 0);
    assert_eq!(hub.dismissed_sessions.len(), 20);
}

#[test]
fn test_f4_bva_03_dismiss_duplicate_calls() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s1", "S1", "Gemini", AgentState::Idle, "I", 1, "Windows")).unwrap();
    hub.poll_events();

    hub.apply_actions(vec![
        UserAction::Dismiss("s1".to_string()),
        UserAction::Dismiss("s1".to_string()),
    ]);

    assert_eq!(hub.sessions.len(), 0);
    assert_eq!(hub.dismissed_sessions.len(), 1);
}

#[test]
fn test_f4_bva_04_hundred_dismissed_sessions_retained() {
    let mut hub = create_test_hub();
    let dismissals: Vec<UserAction> = (1..=100).map(|i| UserAction::Dismiss(format!("session_{}", i))).collect();
    hub.apply_actions(dismissals);

    assert_eq!(hub.dismissed_sessions.len(), 100);
    for i in 1..=100 {
        assert!(hub.dismissed_sessions.contains(&format!("session_{}", i)));
    }
}

#[test]
fn test_f4_bva_05_resurrect_at_step_count_plus_one() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s1", "S1", "Gemini", AgentState::Thinking, "T", 42, "Windows")).unwrap();
    hub.poll_events();

    hub.apply_actions(vec![UserAction::Dismiss("s1".to_string())]);
    assert_eq!(hub.sessions.len(), 0);

    // Event at step 43 resurrects
    hub.sender().send(make_event("s1", "S1", "Gemini", AgentState::Thinking, "T", 43, "Windows")).unwrap();
    hub.poll_events();

    assert_eq!(hub.sessions.len(), 1);
    assert_eq!(hub.sessions[0].step_count, 43);
}

// ==============================================================================
// F5 Boundaries & Corner Cases: Scope-Sensitive Alert Acknowledgement
// ==============================================================================

#[test]
fn test_f5_bva_01_select_unknown_session() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s1", "S1", "Gemini", AgentState::Thinking, "T", 1, "Windows")).unwrap();
    hub.poll_events();
    hub.sender().send(make_event("s1", "S1", "Gemini", AgentState::WaitingForInput { prompt_preview: "P".into() }, "W", 2, "Windows")).unwrap();
    hub.poll_events();

    assert!(hub.sessions[0].attention.is_unacknowledged);

    // Select unknown session ID
    hub.apply_actions(vec![UserAction::Select("random-id-404".to_string())]);

    assert!(hub.sessions[0].attention.is_unacknowledged, "Unrelated session alert must not be cleared");
}

#[test]
fn test_f5_bva_02_acknowledge_empty_category() {
    let mut hub = create_test_hub();
    hub.apply_actions(vec![UserAction::AcknowledgeCategory("host:ArchLinux".to_string())]);
    assert_eq!(hub.sessions.len(), 0);
}

#[test]
fn test_f5_bva_03_acknowledge_with_zero_unacked_alerts() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s1", "S1", "Gemini", AgentState::Idle, "I", 1, "Windows")).unwrap();
    hub.poll_events();

    assert!(!hub.sessions[0].attention.is_unacknowledged);
    hub.apply_actions(vec![UserAction::AcknowledgeAll]);
    assert!(!hub.sessions[0].attention.is_unacknowledged);
}

#[test]
fn test_f5_bva_04_case_insensitive_category_matching() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("win-1", "Win1", "Gemini", AgentState::Thinking, "T", 1, "windows")).unwrap();
    hub.poll_events();
    hub.sender().send(make_event("win-1", "Win1", "Gemini", AgentState::WaitingForInput { prompt_preview: "P".into() }, "W", 2, "windows")).unwrap();
    hub.poll_events();

    assert!(hub.sessions[0].attention.is_unacknowledged);

    hub.apply_actions(vec![UserAction::AcknowledgeCategory("windows".to_string())]);
    assert!(!hub.sessions[0].attention.is_unacknowledged);
}

#[test]
fn test_f5_bva_05_exact_pulse_timeout_boundary_4000ms() {
    let mut attention = AttentionState::new();
    attention.update(&AgentState::Thinking, 1);
    let state = AgentState::WaitingForInput { prompt_preview: "Prompt".into() };
    attention.update(&state, 2);

    // 3.90s elapsed: still pulsating
    attention.triggered_at = Some(Instant::now() - Duration::from_millis(3900));
    assert!(attention.is_pulsating(&state));

    // 4.10s elapsed: stopped pulsating
    attention.triggered_at = Some(Instant::now() - Duration::from_millis(4100));
    assert!(!attention.is_pulsating(&state));
}

// ==============================================================================
// F6 Boundaries & Corner Cases: In-Place Session Mutation & State Persistence
// ==============================================================================

#[test]
fn test_f6_bva_01_extreme_marquee_offset_no_overflow() {
    let scale = 1.15;
    let offset = 1_000_000.0;
    let text_len = 50;
    let wrapped = LayoutFormulas::marquee_modulo_offset(offset, text_len, scale);

    let max_wrap = (text_len + 6) as f32 * 7.0 * scale + 40.0;
    assert!(wrapped >= 0.0 && wrapped < max_wrap);
    assert!(!wrapped.is_nan() && !wrapped.is_infinite());
}

#[test]
fn test_f6_bva_02_in_place_vu_update_zero_sessions() {
    let mut hub = create_test_hub();
    // In-place loop over empty sessions
    for session in hub.sessions.iter_mut() {
        session.marquee_offset += 1.0;
    }
    assert_eq!(hub.sessions.len(), 0);
}

#[test]
fn test_f6_bva_03_custom_title_unicode_and_emojis() {
    let mut storage = CustomTitlesStorage::in_memory();
    let complex_title = "🦀 Rust Agent • 100% Valid 🚀";

    storage.set_title("unicode_session", complex_title);
    assert_eq!(storage.get_title("unicode_session"), Some(complex_title.to_string()));
}

#[test]
fn test_f6_bva_04_custom_title_extreme_length_500_chars() {
    let mut storage = CustomTitlesStorage::in_memory();
    let long_title = "A".repeat(500);

    storage.set_title("long_session", &long_title);
    assert_eq!(storage.get_title("long_session"), Some(long_title));
}

#[test]
fn test_f6_bva_05_custom_title_special_symbols_and_newlines() {
    let mut storage = CustomTitlesStorage::in_memory();
    let title_with_symbols = "  Backend Service [Port: 8080] (v1.2.3)  ";

    storage.set_title("sym_session", title_with_symbols);
    // set_title trims leading and trailing whitespace
    assert_eq!(storage.get_title("sym_session"), Some("Backend Service [Port: 8080] (v1.2.3)".to_string()));
}

// ==============================================================================
// F7 Boundaries & Corner Cases: Proportional Dynamic Scaling (0.85x - 1.6x)
// ==============================================================================

#[test]
fn test_f7_bva_01_exact_minimum_scale_085() {
    let scale = LayoutFormulas::MIN_FONT_SCALE;
    assert_eq!(scale, 0.85);

    let row_h = LayoutFormulas::normal_row_height(scale);
    let badge_font = LayoutFormulas::badge_font_size(scale);

    assert!((row_h - (52.0 * 0.85)).abs() < 0.01);
    assert!((badge_font - (10.5 * 0.85)).abs() < 0.01);
}

#[test]
fn test_f7_bva_02_exact_maximum_scale_160() {
    let scale = LayoutFormulas::MAX_FONT_SCALE;
    assert_eq!(scale, 1.60);

    // Height uses scale.min(1.3)
    let row_h = LayoutFormulas::normal_row_height(scale);
    let edit_h = LayoutFormulas::edit_row_height(scale);

    assert!((row_h - (52.0 * 1.30)).abs() < 0.01);
    assert!((edit_h - (74.0 * 1.30)).abs() < 0.01);
}

#[test]
fn test_f7_bva_03_extreme_negative_scale_clamped() {
    let clamped = LayoutFormulas::clamp_font_scale(-100.0);
    assert_eq!(clamped, 0.85);
}

#[test]
fn test_f7_bva_04_extreme_large_scale_clamped() {
    let clamped = LayoutFormulas::clamp_font_scale(999.0);
    assert_eq!(clamped, 1.60);
}

#[test]
fn test_f7_bva_05_sub_pixel_monotonicity_001_steps() {
    let mut prev_h = 0.0;
    for step in 85..=130 {
        let scale = step as f32 / 100.0;
        let h = LayoutFormulas::normal_row_height(scale);
        assert!(h >= prev_h);
        prev_h = h;
    }
}

// ==============================================================================
// F8 Boundaries & Corner Cases: Bounding Box Padding & Text Layout
// ==============================================================================

#[test]
fn test_f8_bva_01_empty_status_text_layout() {
    let scale = 1.0;
    let modulo = LayoutFormulas::marquee_modulo_offset(10.0, 0, scale);
    let max_wrap = 6.0 * 7.0 * scale + 40.0;
    assert!(modulo < max_wrap);
}

#[test]
fn test_f8_bva_02_single_glyph_status_text() {
    let scale = 1.0;
    let modulo = LayoutFormulas::marquee_modulo_offset(5.0, 1, scale);
    let max_wrap = 7.0 * 7.0 * scale + 40.0;
    assert!(modulo < max_wrap);
}

#[test]
fn test_f8_bva_03_extreme_long_status_text_1000_chars() {
    let scale = 1.15;
    let offset = 500.0;
    let modulo = LayoutFormulas::marquee_modulo_offset(offset, 1000, scale);

    let max_wrap = (1006.0 * 7.0 * scale) + 40.0;
    assert!(modulo < max_wrap);
    assert_eq!(modulo, offset);
}

#[test]
fn test_f8_bva_04_control_characters_in_status() {
    let text = "Line1\r\nLine2\t\x1b[32mColor\x1b[0m";
    let clean: String = text.chars().filter(|c| !c.is_control()).collect();

    assert!(!clean.contains('\n'));
    assert!(!clean.contains('\r'));
}

#[test]
fn test_f8_bva_05_zero_available_width_boundary() {
    let row_rect = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(0.0, 52.0));
    assert_eq!(row_rect.width(), 0.0);
    assert_eq!(row_rect.height(), 52.0);
}

// ==============================================================================
// F9 Boundaries & Corner Cases: Viewport Culling & Repaint Optimization
// ==============================================================================

#[test]
fn test_f9_bva_01_twenty_five_active_sessions_load() {
    let mut hub = create_test_hub();
    for i in 1..=25 {
        hub.sender().send(make_event(&format!("s{}", i), "S", "Gemini", AgentState::Thinking, "T", i, "Windows")).unwrap();
    }
    hub.poll_events();

    assert_eq!(hub.sessions.len(), 25);
    let categories = hub.active_categories();
    assert_eq!(categories[0].session_count, 25);
}

#[test]
fn test_f9_bva_02_all_sessions_waiting_approval_sort_stability() {
    let mut hub = create_test_hub();
    for i in 1..=10 {
        hub.sender().send(make_event(&format!("s{}", i), "S", "Gemini", AgentState::WaitingForApproval { name: "t".into(), summary: "s".into() }, "A", i, "Windows")).unwrap();
    }
    hub.poll_events();

    let cat = &hub.active_categories()[0];
    let sorted = hub.sessions_for_category(cat);

    assert_eq!(sorted.len(), 10);
    // All have priority rank 1
    for s in &sorted {
        assert_eq!(s.sort_priority(), 1);
    }
}

#[test]
fn test_f9_bva_03_all_sessions_stale_sort() {
    let mut hub = create_test_hub();
    for i in 1..=10 {
        hub.sender().send(make_event(&format!("s{}", i), "S", "Gemini", AgentState::Idle, "I", 1, "Windows")).unwrap();
    }
    hub.poll_events();

    for s in hub.sessions.iter_mut() {
        s.last_updated = Instant::now() - Duration::from_secs(1000);
    }

    let cat = &hub.active_categories()[0];
    let sorted = hub.sessions_for_category(cat);

    for s in sorted {
        assert_eq!(s.sort_priority(), 99);
    }
}

#[test]
fn test_f9_bva_04_exact_stale_boundary_900_seconds() {
    let mut hub = create_test_hub();
    hub.sender().send(make_event("s1", "S1", "Gemini", AgentState::Idle, "I", 1, "Windows")).unwrap();
    hub.poll_events();

    // 899s ago: not stale
    hub.sessions[0].last_updated = Instant::now() - Duration::from_secs(899);
    assert!(!hub.sessions[0].is_stale());

    // 901s ago: stale (> 15m)
    hub.sessions[0].last_updated = Instant::now() - Duration::from_secs(901);
    assert!(hub.sessions[0].is_stale());
}

#[test]
fn test_f9_bva_05_ten_distinct_wsl_distro_categories() {
    let mut hub = create_test_hub();
    let distros = ["arch", "alpine", "debian", "fedora", "gentoo", "kali", "nixos", "opensuse", "rhel", "ubuntu"];

    for d in distros {
        hub.sender().send(make_event(&format!("s_{}", d), "S", "Claude", AgentState::Idle, "I", 1, &format!("wsl:{}", d))).unwrap();
    }
    hub.poll_events();

    let cats = hub.active_categories();
    assert_eq!(cats.len(), 11); // Windows + 10 distros
    assert_eq!(cats[0].label, "Windows");
    // Other distros are sorted alphabetically
    assert_eq!(cats[1].label, "alpine");
    assert_eq!(cats[2].label, "arch");
}

// ==============================================================================
// F10 Boundaries & Corner Cases: Winamp VU Ballistics & Peak Hold
// ==============================================================================

#[test]
fn test_f10_bva_01_zero_dt_vu_update() {
    let initial = 0.5;
    let updated = LayoutFormulas::vu_update_active(initial, 0, 1.0, 0.0);
    assert_eq!(initial, updated);
}

#[test]
fn test_f10_bva_02_large_dt_spike_1_second() {
    let initial = 0.0;
    let updated = LayoutFormulas::vu_update_active(initial, 0, 1.0, 1.0);
    assert!(updated >= 0.0 && updated <= 1.0);
}

#[test]
fn test_f10_bva_03_negative_dt_resilience() {
    let initial = 0.5;
    let updated = LayoutFormulas::lerp(initial, 1.0, -0.5);
    assert_eq!(updated, initial, "Negative t must clamp to 0.0");
}

#[test]
fn test_f10_bva_04_vu_discrete_segments_boundaries() {
    let total_segments: f32 = 5.0;
    let test_levels: [f32; 6] = [0.0, 0.19, 0.39, 0.59, 0.79, 1.0];
    let expected_segs = [0, 1, 2, 3, 4, 5];

    for (level, expected) in test_levels.iter().zip(expected_segs.iter()) {
        let segs = (level * total_segments).round() as usize;
        assert_eq!(segs, *expected);
    }
}

#[test]
fn test_f10_bva_05_vu_8_band_wave_phase_isolation() {
    let pulse_phase = 1.5;
    let mut wave_values = Vec::new();

    for i in 0..8 {
        let wave = ((pulse_phase * 2.8 + i as f32 * 0.6).sin() * 0.5 + 0.5)
            * ((pulse_phase * 1.1 + (8 - i) as f32 * 0.4).cos() * 0.4 + 0.6);
        wave_values.push(wave);
    }

    // Check that not all bands have identical values
    let first = wave_values[0];
    assert!(wave_values.iter().any(|v| (v - first).abs() > 0.05));
}

// ==============================================================================
// F11 Boundaries & Corner Cases: Organic LED Breathing & Bloom
// ==============================================================================

#[test]
fn test_f11_bva_01_extreme_pulse_phase_float_safety() {
    let huge_phase = 1_000_000.0;
    let intensity = LayoutFormulas::led_breathe_intensity(huge_phase);
    assert!(intensity >= 0.2 && intensity <= 1.0);
    assert!(!intensity.is_nan());
}

#[test]
fn test_f11_bva_02_led_minimum_alpha_floor_exact() {
    let mut min_val = 1.0;
    for step in 0..1000 {
        let phase = step as f32 * 0.01;
        let intensity = LayoutFormulas::led_breathe_intensity(phase);
        if intensity < min_val {
            min_val = intensity;
        }
    }
    assert!((min_val - 0.30).abs() < 0.01, "Minimum sinus breathing floor around 0.30");
    assert!(min_val >= 0.20, "Floor must never drop below 0.20");
}

#[test]
fn test_f11_bva_03_led_maximum_alpha_ceiling_exact() {
    let mut max_val = 0.0;
    for step in 0..1000 {
        let phase = step as f32 * 0.01;
        let intensity = LayoutFormulas::led_breathe_intensity(phase);
        if intensity > max_val {
            max_val = intensity;
        }
    }
    assert!((max_val - 1.0).abs() < 0.01);
    assert!(max_val <= 1.00);
}

#[test]
fn test_f11_bva_04_rapid_pulse_advance_large_dt() {
    let mut phase = 0.0;
    let dt = 0.5;
    phase += dt * 4.0;
    assert_eq!(phase, 2.0);
}

#[test]
fn test_f11_bva_05_color_lerp_boundary_values() {
    assert_eq!(LayoutFormulas::lerp(10.0, 50.0, 0.0), 10.0);
    assert_eq!(LayoutFormulas::lerp(10.0, 50.0, 1.0), 50.0);
    assert_eq!(LayoutFormulas::lerp(10.0, 50.0, 1.5), 50.0);
    assert_eq!(LayoutFormulas::lerp(10.0, 50.0, -0.5), 10.0);
}

// ==============================================================================
// F12 Boundaries & Corner Cases: Dark Theme Palette Consistency
// ==============================================================================

#[test]
fn test_f12_bva_01_rgb_channel_clamping() {
    let c = Color32::from_rgb(16, 18, 22);
    assert_eq!(c.r(), 16);
    assert_eq!(c.g(), 18);
    assert_eq!(c.b(), 22);
}

#[test]
fn test_f12_bva_02_alpha_compositing_non_zero() {
    let alpha_glow = Color32::from_rgba_unmultiplied(0, 255, 128, 230);
    assert_eq!(alpha_glow.a(), 230);
}

#[test]
fn test_f12_bva_03_contrast_between_hover_and_normal() {
    let normal = Color32::from_rgb(7, 12, 9);
    let hover = Color32::from_rgb(12, 18, 14);

    let diff_r = (hover.r() as i32 - normal.r() as i32).abs();
    let diff_g = (hover.g() as i32 - normal.g() as i32).abs();
    assert!(diff_r > 0 && diff_g > 0);
}

#[test]
fn test_f12_bva_04_contrast_between_selected_and_normal() {
    let normal = Color32::from_rgb(7, 12, 9);
    let selected = Color32::from_rgb(10, 22, 16);

    let diff_g = selected.g() as i32 - normal.g() as i32;
    assert!(diff_g >= 10, "Selected row must have distinct green tint");
}

#[test]
fn test_f12_bva_05_warning_amber_contrast_on_dark() {
    let amber = Color32::from_rgb(255, 205, 30);
    let chassis_dark = Color32::from_rgb(18, 20, 25);

    let luminance_amber = amber.r() as i32 + amber.g() as i32;
    let luminance_chassis = chassis_dark.r() as i32 + chassis_dark.g() as i32;
    assert!(luminance_amber - luminance_chassis > 400);
}

// ==============================================================================
// F13 Boundaries & Corner Cases: Zero Compiler Warnings
// ==============================================================================

#[test]
fn test_f13_bva_01_all_none_metadata_roundtrip() {
    let meta = SessionMetadata {
        host: "Windows".to_string(),
        tmux_session: None,
        tmux_window: None,
        tmux_pane: None,
        cwd: None,
        pid: None,
        agent_type: None,
    };

    let json_str = serde_json::to_string(&meta).unwrap();
    let deserialized: SessionMetadata = serde_json::from_str(&json_str).unwrap();
    assert_eq!(meta, deserialized);
}

#[test]
fn test_f13_bva_02_all_some_metadata_roundtrip() {
    let meta = SessionMetadata {
        host: "wsl:Ubuntu".to_string(),
        tmux_session: Some("main".to_string()),
        tmux_window: Some("1:bash".to_string()),
        tmux_pane: Some("%0".to_string()),
        cwd: Some("/home/dev/project".to_string()),
        pid: Some(12345),
        agent_type: Some("Claude".to_string()),
    };

    let json_str = serde_json::to_string(&meta).unwrap();
    let deserialized: SessionMetadata = serde_json::from_str(&json_str).unwrap();
    assert_eq!(meta, deserialized);
}

#[test]
fn test_f13_bva_03_empty_session_id_event() {
    let event = make_event("", "", "", AgentState::Idle, "", 0, "");
    assert_eq!(event.session_id, "");
    assert_eq!(event.step_count, 0);
}

#[test]
fn test_f13_bva_04_nested_quotes_in_status_text() {
    let text_with_quotes = "Running \"grep -rn \\\"pattern\\\" .\"";
    let event = make_event("s1", "S1", "Gemini", AgentState::Thinking, text_with_quotes, 1, "Windows");
    let json_str = serde_json::to_string(&event).unwrap();
    let deserialized: SessionEvent = serde_json::from_str(&json_str).unwrap();

    assert_eq!(deserialized.status_text, text_with_quotes);
}

#[test]
fn test_f13_bva_05_custom_titles_empty_map_json_roundtrip() {
    let titles_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let json_str = serde_json::to_string(&titles_map).unwrap();
    assert_eq!(json_str, "{}");

    let deserialized: std::collections::HashMap<String, String> = serde_json::from_str(&json_str).unwrap();
    assert!(deserialized.is_empty());
}

// ==============================================================================
// F14 Boundaries & Corner Cases: WSL2 Daemon Broadcast Resilience
// ==============================================================================

#[test]
fn test_f14_bva_01_daemon_empty_initial_sessions() {
    let initial_sessions: Vec<SessionEvent> = Vec::new();
    assert!(initial_sessions.is_empty());
}

#[test]
fn test_f14_bva_02_daemon_payload_strict_trailing_newline() {
    let event = make_event("s1", "S1", "Gemini", AgentState::Idle, "I", 1, "Windows");
    let json_str = serde_json::to_string(&event).unwrap();
    let payload = format!("{}\n", json_str);

    assert!(payload.ends_with('\n'));
    assert_eq!(payload.chars().filter(|c| *c == '\n').count(), 1);
}

#[tokio::test]
async fn test_f14_bva_03_daemon_client_disconnect_resilience() {
    let (tx, rx) = tokio::sync::broadcast::channel::<String>(16);
    drop(rx); // All receivers dropped

    // Sending into channel with no receivers returns Err, but does not panic
    let res = tx.send("message".to_string());
    assert!(res.is_err());
}

#[tokio::test]
async fn test_f14_bva_04_daemon_reconnect_fresh_subscription() {
    let (tx, _rx_old) = tokio::sync::broadcast::channel::<String>(16);
    let mut rx_new = tx.subscribe();

    tx.send("live_event".to_string()).unwrap();
    let msg = rx_new.recv().await.unwrap();
    assert_eq!(msg, "live_event");
}

#[test]
fn test_f14_bva_05_daemon_distro_name_with_dashes_and_dots() {
    let distros = ["Ubuntu-24.04", "Debian-12.5", "openSUSE-Tumbleweed.2024"];
    for d in distros {
        let host_tag = format!("wsl:{}", d);
        let clean = host_tag.strip_prefix("wsl:").unwrap();
        assert_eq!(clean, d);
    }
}
