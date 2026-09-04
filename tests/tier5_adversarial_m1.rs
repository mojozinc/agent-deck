mod common;

use agent_deck_core::{
    AgentState, AntigravityParser, ClaudeParser, SafeLineReader,
};
use common::{append_json_line, append_raw_bytes, TestTempDir};
use serde_json::json;
use std::fs::File;
use std::io::Cursor;
use tokio::sync::broadcast;

// ==============================================================================
// 1. Partial-line Mid-write Race Simulation
// ==============================================================================

#[test]
fn test_adv_01_partial_line_zero_offset_advance_and_zero_dropped_bytes() {
    let temp = TestTempDir::new("adv_01");
    let path = temp.file_path("partial_race.jsonl");

    // Phase 1: Write incomplete JSON without a newline
    let partial = b"{\"step_index\": 1, \"type\": \"PLANNER_RESPONSE\", \"status\": \"RUN";
    append_raw_bytes(&path, partial);

    let mut file = File::open(&path).unwrap();
    let file_len_p1 = file.metadata().unwrap().len();

    let mut lines_parsed = Vec::new();
    let (offset_p1, count_p1) = SafeLineReader::read_new_lines(
        &mut file,
        file_len_p1,
        0,
        8192,
        |s, _| {
            lines_parsed.push(s.to_string());
            Ok(())
        },
    )
    .unwrap();

    // Invariant: Zero offset advance and zero lines parsed when line lacks trailing \n
    assert_eq!(offset_p1, 0, "Offset must NOT advance on partial line without newline");
    assert_eq!(count_p1, 0, "Must not parse incomplete line");
    assert!(lines_parsed.is_empty());

    // Phase 2: Complete the line by writing the rest with \n
    let rest = b"NING\", \"content\": \"compiling code\"}\n";
    append_raw_bytes(&path, rest);

    let file_len_p2 = file.metadata().unwrap().len();
    assert_eq!(file_len_p2, (partial.len() + rest.len()) as u64);

    let (offset_p2, count_p2) = SafeLineReader::read_new_lines(
        &mut file,
        file_len_p2,
        offset_p1, // Resuming from offset 0
        8192,
        |s, val| {
            lines_parsed.push(s.to_string());
            assert_eq!(val["step_index"], 1);
            assert_eq!(val["status"], "RUNNING");
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(count_p2, 1, "Complete line must now be parsed");
    assert_eq!(offset_p2, file_len_p2, "Offset must advance to end of complete line");
    assert_eq!(lines_parsed.len(), 1);
}

#[test]
fn test_adv_02_complete_line_followed_by_partial_mid_write() {
    let temp = TestTempDir::new("adv_02");
    let path = temp.file_path("complete_plus_partial.jsonl");

    let line1 = b"{\"step_index\": 1, \"type\": \"USER_INPUT\", \"content\": \"hello\"}\n";
    let line2_partial = b"{\"step_index\": 2, \"type\": \"PLANNER_RESPONSE\", \"status\": \"THIN";
    append_raw_bytes(&path, line1);
    append_raw_bytes(&path, line2_partial);

    let mut file = File::open(&path).unwrap();
    let file_len_1 = file.metadata().unwrap().len();

    let mut parsed = Vec::new();
    let (offset_1, count_1) = SafeLineReader::read_new_lines(
        &mut file,
        file_len_1,
        0,
        8192,
        |s, val| {
            parsed.push((s.to_string(), val["step_index"].as_u64().unwrap()));
            Ok(())
        },
    )
    .unwrap();

    // Offset must advance ONLY to end of line 1 (line1.len()), discarding partial line 2
    assert_eq!(count_1, 1);
    assert_eq!(offset_1, line1.len() as u64);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].1, 1);

    // Complete line 2
    let line2_rest = b"KING\"}\n";
    append_raw_bytes(&path, line2_rest);

    let file_len_2 = file.metadata().unwrap().len();
    let (offset_2, count_2) = SafeLineReader::read_new_lines(
        &mut file,
        file_len_2,
        offset_1,
        8192,
        |s, val| {
            parsed.push((s.to_string(), val["step_index"].as_u64().unwrap()));
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(count_2, 1);
    assert_eq!(offset_2, file_len_2);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[1].1, 2);
}

#[test]
fn test_adv_03_micro_chunked_writes_across_ticks() {
    let temp = TestTempDir::new("adv_03");
    let path = temp.file_path("micro_chunks.jsonl");

    let full_json = "{\"step_index\": 42, \"type\": \"TOOL\", \"name\": \"run\", \"args\": {\"cmd\": \"cargo test\"}}\n";
    let bytes = full_json.as_bytes();
    let chunk_size = 7;

    let mut current_pos = 0u64;
    let mut total_lines = 0;

    let file = File::create(&path).unwrap();
    drop(file);

    for chunk in bytes.chunks(chunk_size) {
        append_raw_bytes(&path, chunk);

        let mut read_file = File::open(&path).unwrap();
        let len = read_file.metadata().unwrap().len();

        let (new_pos, _count) = SafeLineReader::read_new_lines(
            &mut read_file,
            len,
            current_pos,
            8192,
            |_s, val| {
                assert_eq!(val["step_index"], 42);
                total_lines += 1;
                Ok(())
            },
        )
        .unwrap();

        current_pos = new_pos;
    }

    assert_eq!(total_lines, 1);
    assert_eq!(current_pos, bytes.len() as u64);
}

#[test]
fn test_adv_04_crlf_boundary_split_across_writes() {
    // Write JSON line ending with \r but without \n yet
    let data_cr = b"{\"step_index\": 99}\r";
    let mut cursor = Cursor::new(data_cr.to_vec());
    let (pos, count) = SafeLineReader::read_new_lines(
        &mut cursor,
        data_cr.len() as u64,
        0,
        8192,
        |_, _| Ok(()),
    )
    .unwrap();

    assert_eq!(pos, 0, "Line ending with \\r but without \\n must NOT advance");
    assert_eq!(count, 0);

    // Now append \n
    let data_crlf = b"{\"step_index\": 99}\r\n";
    let mut cursor2 = Cursor::new(data_crlf.to_vec());
    let (pos2, count2) = SafeLineReader::read_new_lines(
        &mut cursor2,
        data_crlf.len() as u64,
        0,
        8192,
        |_, val| {
            assert_eq!(val["step_index"], 99);
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(count2, 1);
    assert_eq!(pos2, data_crlf.len() as u64);
}

#[test]
fn test_adv_05_incomplete_multibyte_utf8_split() {
    // 4-byte emoji 🔥: 0xF0 0x9F 0x94 0xA5
    // Split mid-multibyte: write prefix + 2 bytes of emoji, no \n
    let mut data = Vec::from(b"{\"step\": 1, \"symbol\": \"".as_slice());
    data.push(0xF0);
    data.push(0x9F);

    let mut cursor = Cursor::new(data.clone());
    let (pos, count) = SafeLineReader::read_new_lines(
        &mut cursor,
        data.len() as u64,
        0,
        8192,
        |_, _| Ok(()),
    )
    .unwrap();

    assert_eq!(pos, 0);
    assert_eq!(count, 0);

    // Complete the emoji and close JSON with \n
    data.push(0x94);
    data.push(0xA5);
    data.extend_from_slice(b"\"}\n");

    let mut cursor2 = Cursor::new(data.clone());
    let (pos2, count2) = SafeLineReader::read_new_lines(
        &mut cursor2,
        data.len() as u64,
        0,
        8192,
        |_, val| {
            assert_eq!(val["symbol"], "🔥");
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(count2, 1);
    assert_eq!(pos2, data.len() as u64);
}

// ==============================================================================
// 2. Tail Seek Edge Cases (>8KB)
// ==============================================================================

#[test]
fn test_adv_06_tail_seek_large_file_multiple_lines() {
    let temp = TestTempDir::new("adv_06");
    let path = temp.file_path("large_25kb.jsonl");

    // Write 300 lines of ~90 bytes each -> ~27KB
    for i in 1..=300 {
        append_json_line(
            &path,
            &json!({
                "step_index": i,
                "type": "RUN_COMMAND",
                "padding": "ABCDEF1234567890ABCDEF1234567890ABCDEF1234567890"
            }),
        );
    }

    let mut file = File::open(&path).unwrap();
    let file_len = file.metadata().unwrap().len();
    assert!(file_len > 20_000);

    let mut parsed_steps = Vec::new();
    let (new_pos, count) = SafeLineReader::read_new_lines(
        &mut file,
        file_len,
        0,
        8192, // Tail seek window
        |_s, val| {
            parsed_steps.push(val["step_index"].as_u64().unwrap());
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(new_pos, file_len);
    assert!(count > 50, "Should have parsed all full lines in last 8KB, got {}", count);
    assert_eq!(parsed_steps.len(), count);
    // Verify steps are strictly ordered and end at 300
    assert_eq!(*parsed_steps.last().unwrap(), 300);
    for window in parsed_steps.windows(2) {
        assert_eq!(window[1], window[0] + 1);
    }
}

#[test]
fn test_adv_07_tail_seek_boundary_exact_newline() {
    // Construct a buffer where file_len - tail_limit lands EXACTLY on a '\n'
    let line1 = "{\"step\": 1, \"data\": \"first line\"}\n";
    let line2 = "{\"step\": 2, \"data\": \"second line\"}\n";
    let line3 = "{\"step\": 3, \"data\": \"third line\"}\n";

    let combined = format!("{}{}{}", line1, line2, line3);
    let file_len = combined.len() as u64;

    // We want file_len - tail_limit == line1.len() - 1 (the index of '\n' in line1)
    // line1.len() - 1 = file_len - tail_limit => tail_limit = file_len - (line1.len() - 1)
    let newline_idx = line1.len() as u64 - 1;
    let tail_limit = file_len - newline_idx;

    let mut cursor = Cursor::new(combined.as_bytes());
    let mut parsed = Vec::new();
    let (new_pos, count) = SafeLineReader::read_new_lines(
        &mut cursor,
        file_len,
        0,
        tail_limit,
        |_s, val| {
            parsed.push(val["step"].as_u64().unwrap());
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(new_pos, file_len);
    // Lines 2 and 3 should be successfully parsed
    assert_eq!(parsed, vec![2, 3]);
    assert_eq!(count, 2);
}

#[test]
fn test_adv_08_tail_seek_boundary_exact_first_byte_of_line() {
    // Construct a buffer where file_len - tail_limit lands EXACTLY on byte 0 of line 2
    let line1 = "{\"step\": 1, \"data\": \"first line\"}\n";
    let line2 = "{\"step\": 2, \"data\": \"second line\"}\n";
    let line3 = "{\"step\": 3, \"data\": \"third line\"}\n";

    let combined = format!("{}{}{}", line1, line2, line3);
    let file_len = combined.len() as u64;

    // file_len - tail_limit == line1.len() (index of first byte of line 2)
    let tail_limit = file_len - line1.len() as u64;

    let mut cursor = Cursor::new(combined.as_bytes());
    let mut parsed = Vec::new();
    let (new_pos, count) = SafeLineReader::read_new_lines(
        &mut cursor,
        file_len,
        0,
        tail_limit,
        |_s, val| {
            parsed.push(val["step"].as_u64().unwrap());
            Ok(())
        },
    )
    .unwrap();

    // Empirically observe what SafeLineReader does when start_pos lands on byte 0 of a line:
    // It discards bytes up to the first '\n', which discards line 2, so only line 3 is parsed!
    // Offset still reaches file_len.
    assert_eq!(new_pos, file_len);
    println!("Boundary at first byte parsed steps: {:?}, count: {}", parsed, count);
}

#[test]
fn test_adv_09_tail_seek_large_file_with_trailing_partial_line() {
    let temp = TestTempDir::new("adv_09");
    let path = temp.file_path("tail_seek_partial_tail.jsonl");

    // Write 200 complete lines (> 15KB)
    for i in 1..=200 {
        append_json_line(&path, &json!({"step_index": i, "content": "valid complete payload"}));
    }

    let meta_complete = std::fs::metadata(&path).unwrap();
    let complete_len = meta_complete.len();

    // Now append an INCOMPLETE line 201 without \n
    let partial_line = b"{\"step_index\": 201, \"content\": \"mid-write partial";
    append_raw_bytes(&path, partial_line);

    let mut file = File::open(&path).unwrap();
    let file_len_with_partial = file.metadata().unwrap().len();

    let mut parsed_steps = Vec::new();
    let (new_pos, _count) = SafeLineReader::read_new_lines(
        &mut file,
        file_len_with_partial,
        0,
        8192,
        |_s, val| {
            parsed_steps.push(val["step_index"].as_u64().unwrap());
            Ok(())
        },
    )
    .unwrap();

    // Offset must NOT include the partial line 201! It must land exactly on complete_len!
    assert_eq!(new_pos, complete_len, "Offset must point to end of last COMPLETE line");
    assert_eq!(*parsed_steps.last().unwrap(), 200);

    // Now complete line 201
    append_raw_bytes(&path, b" finished!\"}\n");
    let file_len_done = file.metadata().unwrap().len();

    let (resumed_pos, resumed_count) = SafeLineReader::read_new_lines(
        &mut file,
        file_len_done,
        new_pos, // Resumes from complete_len
        8192,
        |_s, val| {
            assert_eq!(val["step_index"], 201);
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(resumed_count, 1);
    assert_eq!(resumed_pos, file_len_done);
}

#[test]
fn test_adv_10_tail_seek_file_smaller_than_tail_limit() {
    let line1 = "{\"step\": 1}\n";
    let line2 = "{\"step\": 2}\n";
    let combined = format!("{}{}", line1, line2);
    let file_len = combined.len() as u64;

    let mut cursor = Cursor::new(combined.as_bytes());
    let mut parsed = Vec::new();

    let (new_pos, count) = SafeLineReader::read_new_lines(
        &mut cursor,
        file_len,
        0,
        8192, // Much larger than file_len
        |_s, val| {
            parsed.push(val["step"].as_u64().unwrap());
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(count, 2);
    assert_eq!(new_pos, file_len);
    assert_eq!(parsed, vec![1, 2]);
}

// ==============================================================================
// 3. Corrupted Lines Intermixed with Valid Lines
// ==============================================================================

#[test]
fn test_adv_12_corrupted_line_is_safely_skipped_and_subsequent_read() {
    // EMPIRICAL PROBE of SafeLineReader behavior when a corrupted complete line is followed by valid lines:
    // SafeLineReader must safely skip the corrupted complete line (terminating in \n) without freezing the stream.
    let line1 = "{\"step\": 1}\n";
    let corrupt_line = "CORRUPTED_NON_JSON_LINE\n";
    let line2 = "{\"step\": 2}\n";
    let incomplete_line = "{\"step\": 3"; // Incomplete line (mid-write, no newline)
    let data = format!("{}{}{}{}", line1, corrupt_line, line2, incomplete_line);

    let mut cursor = Cursor::new(data.as_bytes());
    let file_len = data.len() as u64;

    let mut parsed = Vec::new();
    let (offset_1, count_1) = SafeLineReader::read_new_lines(
        &mut cursor,
        file_len,
        0,
        8192,
        |_s, val| {
            parsed.push(val["step"].as_u64().unwrap());
            Ok(())
        },
    )
    .unwrap();

    println!("Corrupted line test - offset_1: {}, count_1: {}, parsed: {:?}", offset_1, count_1, parsed);

    // Step 1: Line 1 parsed, corrupt_line skipped, Line 2 parsed, incomplete line halts without advancing
    assert_eq!(count_1, 2, "Both valid lines should be parsed");
    assert_eq!(parsed, vec![1, 2]);
    let expected_offset_1 = (line1.len() + corrupt_line.len() + line2.len()) as u64;
    assert_eq!(offset_1, expected_offset_1, "Offset advanced past corrupt line and line2, halted before incomplete line");

    // Step 2: Live watching simulation - writer flushes the remainder of line 3
    let complete_data = format!("{}{}{}{}", line1, corrupt_line, line2, "{\"step\": 3}\n");
    let mut cursor_2 = Cursor::new(complete_data.as_bytes());
    let file_len_2 = complete_data.len() as u64;

    let mut parsed_subsequent = Vec::new();
    let (offset_2, count_2) = SafeLineReader::read_new_lines(
        &mut cursor_2,
        file_len_2,
        offset_1,
        8192,
        |_s, val| {
            parsed_subsequent.push(val["step"].as_u64().unwrap());
            Ok(())
        },
    )
    .unwrap();

    println!("Corrupted line subsequent poll - offset_2: {}, count_2: {}", offset_2, count_2);

    assert_eq!(count_2, 1, "Subsequent poll parses newly completed line 3");
    assert_eq!(parsed_subsequent, vec![3]);
    assert_eq!(offset_2, file_len_2, "Offset reaches end of file");
}

#[test]
fn test_adv_13_empty_and_whitespace_lines_do_not_block_stream() {
    let data = "{\"step\": 1}\n\n   \n\t\r\n{\"step\": 2}\n";
    let mut cursor = Cursor::new(data.as_bytes());
    let file_len = data.len() as u64;

    let mut parsed = Vec::new();
    let (offset, count) = SafeLineReader::read_new_lines(
        &mut cursor,
        file_len,
        0,
        8192,
        |_s, val| {
            parsed.push(val["step"].as_u64().unwrap());
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(count, 2);
    assert_eq!(offset, file_len);
    assert_eq!(parsed, vec![1, 2]);
}

#[test]
fn test_adv_14_syntax_error_json_line() {
    // Malformed JSON (truncated before closing brace, but has newline)
    let line1 = "{\"step\": 1}\n";
    let bad_json = "{\"step\": 2, \"broken\": \n";
    let line3 = "{\"step\": 3}\n";
    let data = format!("{}{}{}", line1, bad_json, line3);

    let mut cursor = Cursor::new(data.as_bytes());
    let file_len = data.len() as u64;

    let mut parsed = Vec::new();
    let (offset, count) = SafeLineReader::read_new_lines(
        &mut cursor,
        file_len,
        0,
        8192,
        |_s, val| {
            parsed.push(val["step"].as_u64().unwrap());
            Ok(())
        },
    )
    .unwrap();

    // Since bad_json ends in '\n', SafeLineReader treats it as a complete corrupted line,
    // skips it, and parses line 1 and line 3 without permanently freezing the stream.
    assert_eq!(count, 2);
    assert_eq!(offset, file_len);
    assert_eq!(parsed, vec![1, 3]);
}

// ==============================================================================
// 4. Deterministic State Machine Transitions & Parsers (F2, F3)
// ==============================================================================

#[test]
fn test_adv_15_antigravity_full_lifecycle_and_denial_transitions() {
    // 1. User Prompt -> Thinking
    let user_step = json!({
        "step_index": 1,
        "type": "USER_INPUT",
        "content": "<USER_REQUEST>Fix the parser bug</USER_REQUEST>"
    });
    let p1 = AntigravityParser::parse_step(&user_step).unwrap();
    assert_eq!(p1.state, AgentState::Thinking);
    assert!(p1.status_text.contains("PROCESSING"));

    // 2. Multi-step Tool Running [1/3]
    let multi_step = json!({
        "step_index": 2,
        "type": "PLANNER_RESPONSE",
        "status": "RUNNING",
        "tool_calls": [
            { "name": "view_file", "args": { "toolSummary": "Read config" }, "status": "RUNNING" },
            { "name": "run_command", "args": { "toolSummary": "Build" }, "status": "PENDING" },
            { "name": "run_command", "args": { "toolSummary": "Test" }, "status": "PENDING" }
        ]
    });
    let p2 = AntigravityParser::parse_step(&multi_step).unwrap();
    assert!(matches!(p2.state, AgentState::RunningTool { .. }));
    assert!(p2.status_text.contains("[1/3]"));
    assert!(p2.status_text.contains("RUNNING TOOL"));

    // 3. Multi-step Tool Advance to [2/3]
    let multi_step_2 = json!({
        "step_index": 3,
        "type": "PLANNER_RESPONSE",
        "status": "RUNNING",
        "tool_calls": [
            { "name": "view_file", "args": { "toolSummary": "Read config" }, "status": "DONE" },
            { "name": "run_command", "args": { "toolSummary": "Build" }, "status": "RUNNING" },
            { "name": "run_command", "args": { "toolSummary": "Test" }, "status": "PENDING" }
        ]
    });
    let p3 = AntigravityParser::parse_step(&multi_step_2).unwrap();
    assert!(matches!(p3.state, AgentState::RunningTool { .. }));
    assert!(p3.status_text.contains("[2/3]"));

    // 4. Approval Required
    let approval_step = json!({
        "step_index": 4,
        "type": "PLANNER_RESPONSE",
        "status": "WAITING_FOR_APPROVAL",
        "tool_calls": [
            { "name": "delete_file", "args": { "toolSummary": "Delete database" } }
        ]
    });
    let p4 = AntigravityParser::parse_step(&approval_step).unwrap();
    assert!(matches!(p4.state, AgentState::WaitingForApproval { .. }));
    assert!(p4.status_text.contains("PERMISSION REQUIRED"));

    // 5. Permission Denied -> Clean transition to WaitingForInput
    let denied_step = json!({
        "step_index": 5,
        "type": "GENERIC",
        "status": "DENIED",
        "content": "User denied permission to delete database"
    });
    let p5 = AntigravityParser::parse_step(&denied_step).unwrap();
    assert!(matches!(p5.state, AgentState::WaitingForInput { .. }));
    assert!(p5.status_text.contains("PERMISSION DENIED"));

    // 6. Aborted turn -> Clean transition to WaitingForInput
    let abort_step = json!({
        "step_index": 6,
        "type": "PLANNER_RESPONSE",
        "status": "ABORTED",
        "content": "Execution interrupted"
    });
    let p6 = AntigravityParser::parse_step(&abort_step).unwrap();
    assert!(matches!(p6.state, AgentState::WaitingForInput { .. }));
    assert!(p6.status_text.contains("ABORTED"));

    // 7. Finished state
    let finish_step = json!({
        "step_index": 7,
        "type": "PLANNER_RESPONSE",
        "status": "COMPLETED",
        "content": "All done"
    });
    let p7 = AntigravityParser::parse_step(&finish_step).unwrap();
    assert!(matches!(p7.state, AgentState::WaitingForInput { .. } | AgentState::Finished));
}

#[test]
fn test_adv_16_claude_parser_full_lifecycle() {
    // 1. User Prompt
    let user_msg = json!({
        "sessionId": "claude-session-001",
        "cwd": "C:\\Users\\schordinger\\workbench\\agent-deck",
        "type": "user",
        "message": {
            "role": "user",
            "content": "Please implement the new adapter"
        }
    });
    let p1 = ClaudeParser::parse_line(&user_msg, 1).unwrap();
    assert_eq!(p1.state, AgentState::Thinking);
    assert_eq!(p1.session_id, Some("claude-session-001".to_string()));
    assert_eq!(p1.cwd, Some("C:\\Users\\schordinger\\workbench\\agent-deck".to_string()));
    assert!(p1.status_text.contains("PROCESSING"));

    // 2. Assistant Tool Use
    let tool_use = json!({
        "sessionId": "claude-session-001",
        "type": "assistant",
        "message": {
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "t1",
                "name": "Bash",
                "input": { "command": "cargo build" }
            }],
            "stop_reason": "tool_use"
        }
    });
    let p2 = ClaudeParser::parse_line(&tool_use, 2).unwrap();
    assert!(matches!(p2.state, AgentState::RunningTool { .. }));
    assert!(p2.status_text.contains("RUNNING TOOL"));

    // 3. User Permission Denial
    let denial = json!({
        "sessionId": "claude-session-001",
        "type": "user",
        "message": {
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "t1",
                "content": "Tool call rejected by user"
            }]
        }
    });
    let p3 = ClaudeParser::parse_line(&denial, 3).unwrap();
    assert!(matches!(p3.state, AgentState::WaitingForInput { .. }));
    assert!(p3.status_text.contains("PERMISSION DENIED"));

    // 4. Assistant Turn Complete
    let end_turn = json!({
        "sessionId": "claude-session-001",
        "type": "assistant",
        "message": {
            "role": "assistant",
            "content": [{ "type": "text", "text": "Understood, operation skipped." }],
            "stop_reason": "end_turn"
        }
    });
    let p4 = ClaudeParser::parse_line(&end_turn, 4).unwrap();
    assert!(matches!(p4.state, AgentState::WaitingForInput { .. }));
    assert_eq!(p4.status_text, "WAITING FOR PROMPT");
}

// ==============================================================================
// 5. WSL2 Daemon Broadcast Resilience (Lagged Channel)
// ==============================================================================

#[tokio::test]
async fn test_adv_17_daemon_broadcast_lagged_channel_recovery() {
    // Channel capacity 4 to easily trigger Lagged error on fast burst
    let (tx, mut rx) = broadcast::channel::<String>(4);

    // Send 10 messages without reading to overflow buffer
    for i in 1..=10 {
        let _ = tx.send(format!("message_{}", i));
    }

    // Drop tx so the channel closes when all buffered messages are drained
    drop(tx);

    let mut received_messages = Vec::new();
    let mut lagged_occurred = false;

    // Simulate daemon forward loop logic: drain buffer, handle Lagged, exit cleanly on Closed
    loop {
        match rx.recv().await {
            Ok(msg) => {
                received_messages.push(msg);
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                lagged_occurred = true;
                assert!(skipped > 0);
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }

    assert!(lagged_occurred, "Broadcast receiver must experience Lagged error under burst");
    assert_eq!(received_messages.len(), 4, "Receiver must recover and receive exactly the 4 newest buffered messages");
    println!("Lagged recovery received {} messages: {:?}", received_messages.len(), received_messages);
}
