use crate::AgentState;
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::SystemTime;

/// Safe line reader that strictly prevents partial-line byte offset corruption.
///
/// Invariants:
/// 1. Advances file offset ONLY to the byte position immediately following the last valid newline `\n`.
/// 2. When tail-seeking (`last_pos == 0 && file_len > tail_limit`), discards the first incomplete line up to the first `\n`.
/// 3. Incomplete lines (those not ending in `\n`) halt without advancing `consumed_offset` (safe mid-write handling).
/// 4. Complete lines (ending in `\n`) that fail JSON deserialization or UTF-8 decoding are safely skipped
///    to prevent permanently freezing the stream on corrupted records.
pub struct SafeLineReader;

impl SafeLineReader {
    /// Reads new complete lines from `file` starting from `last_pos`.
    ///
    /// Returns `Ok((new_offset, lines_parsed))` where `new_offset` is the byte position
    /// immediately following the last successfully parsed complete newline.
    pub fn read_new_lines<R, F>(
        file: &mut R,
        file_len: u64,
        last_pos: u64,
        tail_limit: u64,
        mut on_line: F,
    ) -> std::io::Result<(u64, usize)>
    where
        R: Read + Seek,
        F: FnMut(&str, &Value) -> Result<(), ()>,
    {
        if file_len <= last_pos && last_pos > 0 {
            return Ok((last_pos, 0));
        }

        let is_tail_seeking = last_pos == 0 && file_len > tail_limit;
        let start_pos = if is_tail_seeking {
            file_len.saturating_sub(tail_limit)
        } else {
            last_pos
        };

        file.seek(SeekFrom::Start(start_pos))?;
        let mut reader = BufReader::new(file);
        let mut current_offset = start_pos;

        // If tail-seeking, discard the first partial line up to the first \n
        if is_tail_seeking {
            let mut discard_buf = Vec::new();
            let bytes_skipped = reader.read_until(b'\n', &mut discard_buf)?;
            if bytes_skipped == 0 {
                return Ok((last_pos, 0));
            }
            if !discard_buf.ends_with(b"\n") {
                // Incomplete line spanning the entire tail buffer
                return Ok((last_pos, 0));
            }
            current_offset += bytes_skipped as u64;
        }

        let mut consumed_offset = current_offset;
        let mut line_count = 0;
        let mut line_buf = Vec::new();

        loop {
            line_buf.clear();
            let _line_start_pos = current_offset;
            let bytes_read = reader.read_until(b'\n', &mut line_buf)?;

            if bytes_read == 0 {
                // EOF reached
                break;
            }

            // Invariant 1: Advance file offset ONLY to byte position immediately following last valid \n
            if !line_buf.ends_with(b"\n") {
                // Mid-line EOF reached: line is not completely flushed by writer yet!
                // Do NOT advance consumed_offset past line_start_pos.
                break;
            }

            current_offset += bytes_read as u64;

            let line_str = match std::str::from_utf8(&line_buf) {
                Ok(s) => s,
                Err(_) => {
                    // Complete line ending in \n contains invalid UTF-8 bytes.
                    // Advance consumed_offset past this corrupted line to avoid freezing the stream.
                    consumed_offset = current_offset;
                    continue;
                }
            };

            let trimmed = line_str.trim();
            if trimmed.is_empty() {
                consumed_offset = current_offset;
                continue;
            }

            // Invariant 4: Skip complete non-JSON lines to prevent stream freeze while preserving mid-write safety
            match serde_json::from_str::<Value>(trimmed) {
                Ok(json_val) => {
                    if on_line(trimmed, &json_val).is_ok() {
                        consumed_offset = current_offset;
                        line_count += 1;
                    } else {
                        // Consumer callback reported line could not be handled; halt without advancing past it
                        break;
                    }
                }
                Err(_) => {
                    // Complete line terminating with '\n' failed JSON deserialization (corrupted or non-JSON line).
                    // Advance consumed_offset past this line so we do not permanently freeze the stream,
                    // allowing subsequent valid lines in transcript.jsonl to be processed.
                    consumed_offset = current_offset;
                }
            }
        }

        Ok((consumed_offset, line_count))
    }
}

/// Parsed Antigravity CLI step result.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedTranscriptStep {
    pub step_index: u32,
    pub state: AgentState,
    pub status_text: String,
}

/// Deterministic parser and state machine for Antigravity CLI transcripts.
pub struct AntigravityParser;

impl AntigravityParser {
    pub fn parse_step(json: &Value) -> Option<ParsedTranscriptStep> {
        let step_index = json.get("step_index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let step_type = json.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let source = json.get("source").and_then(|v| v.as_str()).unwrap_or("");
        let status = json.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let content = json.get("content").and_then(|v| v.as_str()).unwrap_or("");

        let status_upper = status.to_ascii_uppercase();

        // 1. Aborted / Cancelled / Interrupted turns -> Transition cleanly to WaitingForInput
        if status_upper == "ABORTED"
            || status_upper == "CANCELLED"
            || status_upper == "CANCELED"
            || status_upper == "INTERRUPTED"
            || step_type.to_ascii_uppercase() == "ABORTED"
        {
            return Some(ParsedTranscriptStep {
                step_index,
                state: AgentState::WaitingForInput {
                    prompt_preview: "Query aborted by user".to_string(),
                },
                status_text: "ABORTED • WAITING FOR PROMPT".to_string(),
            });
        }

        // 2. Permission denials -> Transition cleanly to WaitingForInput (never stuck in Thinking)
        if status_upper == "DENIED"
            || status_upper == "PERMISSION_DENIED"
            || status_upper == "REJECTED"
            || status_upper == "DECLINED"
            || content.contains("Permission denied")
            || content.contains("permission denied")
            || content.contains("Tool call rejected by user")
            || content.contains("declined by user")
            || content.contains("User denied permission")
            || content.contains("Permission rejected")
        {
            return Some(ParsedTranscriptStep {
                step_index,
                state: AgentState::WaitingForInput {
                    prompt_preview: "Permission denied - ready for next instruction".to_string(),
                },
                status_text: "PERMISSION DENIED • WAITING FOR PROMPT".to_string(),
            });
        }

        // 3. Multi-step and active tool calls
        if let Some(tool_calls) = json.get("tool_calls").and_then(|v| v.as_array()) {
            if !tool_calls.is_empty() {
                let total_tools = tool_calls.len();

                // Find active tool (first non-completed tool call or default to 0)
                let (active_idx, active_tool) = tool_calls
                    .iter()
                    .enumerate()
                    .find(|(_, t)| {
                        let t_status = t
                            .get("status")
                            .and_then(|s| s.as_str())
                            .unwrap_or("")
                            .to_ascii_uppercase();
                        t_status != "DONE" && t_status != "COMPLETED"
                    })
                    .unwrap_or((0, &tool_calls[0]));

                let tool_name = active_tool
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool");
                let tool_summary = active_tool
                    .get("args")
                    .and_then(|a| a.get("toolSummary"))
                    .and_then(|s| s.as_str())
                    .unwrap_or(tool_name);
                let tool_action = active_tool
                    .get("args")
                    .and_then(|a| a.get("toolAction"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("");

                let tool_status = active_tool
                    .get("status")
                    .and_then(|s| s.as_str())
                    .unwrap_or(status);
                let tool_status_upper = tool_status.to_ascii_uppercase();

                // Deterministic check: WaitingForApproval vs RunningTool
                let is_approval_required = tool_status_upper == "WAITING_FOR_APPROVAL"
                    || tool_status_upper == "APPROVAL_REQUIRED"
                    || tool_status_upper == "WAITING_APPROVAL"
                    || tool_status_upper == "PENDING_APPROVAL"
                    || tool_status_upper == "CONFIRMATION_REQUIRED"
                    || active_tool
                        .get("requires_approval")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    || active_tool
                        .get("approval_required")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    || tool_name == "ask_question"
                    || tool_name == "ask_user"
                    || tool_name == "confirm"
                    || tool_name == "request_permission"
                    || tool_name == "confirmation"
                    || tool_action.to_ascii_lowercase().contains("permission")
                    || tool_action.to_ascii_lowercase().contains("approval")
                    || tool_action.to_ascii_lowercase().contains("confirmation");

                let step_progress_str = if total_tools > 1 {
                    format!(" [{}/{}]", active_idx + 1, total_tools)
                } else {
                    String::new()
                };

                let (state, status_text) = if is_approval_required {
                    (
                        AgentState::WaitingForApproval {
                            name: tool_name.to_string(),
                            summary: tool_summary.to_string(),
                        },
                        if tool_action.is_empty() {
                            format!(
                                "PERMISSION REQUIRED{}: {} - {}",
                                step_progress_str, tool_name, tool_summary
                            )
                        } else {
                            format!(
                                "PERMISSION REQUIRED{}: {} ({})",
                                step_progress_str, tool_summary, tool_action
                            )
                        },
                    )
                } else {
                    (
                        AgentState::RunningTool {
                            name: tool_name.to_string(),
                            summary: tool_summary.to_string(),
                        },
                        if tool_action.is_empty() {
                            format!(
                                "RUNNING TOOL{}: {} - {}",
                                step_progress_str, tool_name, tool_summary
                            )
                        } else {
                            format!(
                                "RUNNING TOOL{}: {} ({})",
                                step_progress_str, tool_summary, tool_action
                            )
                        },
                    )
                };

                return Some(ParsedTranscriptStep {
                    step_index,
                    state,
                    status_text,
                });
            }
        }

        // 4. Direct tool execution step types (e.g. RUN_COMMAND, VIEW_FILE, CODE_ACTION)
        let is_direct_tool_step = matches!(
            step_type,
            "RUN_COMMAND"
                | "VIEW_FILE"
                | "SEARCH_WEB"
                | "CODE_ACTION"
                | "LIST_DIRECTORY"
                | "TOOL_EXECUTION"
        );
        if is_direct_tool_step
            && (status_upper == "RUNNING"
                || status_upper == "IN_PROGRESS"
                || status_upper.is_empty())
        {
            let preview: String = content
                .lines()
                .filter(|l| !l.trim().is_empty())
                .take(2)
                .collect::<Vec<_>>()
                .join(" ");
            let preview_short: String = preview.chars().take(60).collect();
            return Some(ParsedTranscriptStep {
                step_index,
                state: AgentState::RunningTool {
                    name: step_type.to_lowercase(),
                    summary: preview_short.clone(),
                },
                status_text: format!("RUNNING {}: {}", step_type, preview_short),
            });
        }

        // 5. User Input
        if step_type == "USER_INPUT" || source == "USER_EXPLICIT" {
            let clean_content = content
                .lines()
                .filter(|l| !l.starts_with('<') && !l.ends_with('>'))
                .collect::<Vec<&str>>()
                .join(" ");
            let preview: String = clean_content.chars().take(60).collect();
            return Some(ParsedTranscriptStep {
                step_index,
                state: AgentState::Thinking,
                status_text: format!("PROCESSING: {}", preview),
            });
        }

        // 6. Planner response finished without tool calls -> Waiting for next input
        if step_type == "PLANNER_RESPONSE"
            && (status_upper == "DONE" || status_upper == "COMPLETED")
        {
            return Some(ParsedTranscriptStep {
                step_index,
                state: AgentState::WaitingForInput {
                    prompt_preview: "Ready for input".to_string(),
                },
                status_text: "WAITING FOR PROMPT".to_string(),
            });
        }

        // 7. Explicit completion
        if status_upper == "FINISHED" || status_upper == "COMPLETED" {
            return Some(ParsedTranscriptStep {
                step_index,
                state: AgentState::Finished,
                status_text: "ALL TASKS COMPLETED".to_string(),
            });
        }

        // 8. Explicit error message
        if step_type == "ERROR" || step_type == "ERROR_MESSAGE" {
            let preview: String = content.chars().take(60).collect();
            return Some(ParsedTranscriptStep {
                step_index,
                state: AgentState::Error {
                    message: preview.clone(),
                },
                status_text: format!("ERROR: {}", preview),
            });
        }

        // 9. Default Thinking state
        Some(ParsedTranscriptStep {
            step_index,
            state: AgentState::Thinking,
            status_text: format!("STEP #{}: {}", step_index, step_type),
        })
    }
}

/// Parsed Claude Code step result.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedClaudeStep {
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub step_index: u32,
    pub state: AgentState,
    pub status_text: String,
    pub prompt_preview: Option<String>,
}

/// Parser and state machine for Claude Code Anthropic Messages JSON transcripts.
pub struct ClaudeParser;

impl ClaudeParser {
    pub fn parse_line(json: &Value, step_index: u32) -> Option<ParsedClaudeStep> {
        let entry_type = json.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let session_id = json
            .get("sessionId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let cwd = json
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        if entry_type == "file-history-snapshot" {
            return None;
        }

        let message = json.get("message");
        let role = message
            .and_then(|m| m.get("role"))
            .and_then(|v| v.as_str())
            .unwrap_or(entry_type);
        let stop_reason = message
            .and_then(|m| m.get("stop_reason"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // 1. Assistant message
        if role == "assistant" {
            let stop_upper = stop_reason.to_ascii_uppercase();
            if stop_upper == "CANCELLED"
                || stop_upper == "CANCELED"
                || stop_upper == "INTERRUPTED"
                || stop_upper == "ABORTED"
            {
                return Some(ParsedClaudeStep {
                    session_id,
                    cwd,
                    step_index,
                    state: AgentState::WaitingForInput {
                        prompt_preview: "Query aborted by user".to_string(),
                    },
                    status_text: "ABORTED • WAITING FOR PROMPT".to_string(),
                    prompt_preview: None,
                });
            }

            let content_blocks = message
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array());

            if let Some(blocks) = content_blocks {
                let tool_uses: Vec<&Value> = blocks
                    .iter()
                    .filter(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_use"))
                    .collect();

                if !tool_uses.is_empty() {
                    let total_tools = tool_uses.len();
                    let first_tool = tool_uses[0];
                    let tool_name = first_tool
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("tool");

                    let tool_summary = if let Some(input) = first_tool.get("input") {
                        if let Some(desc) = input.get("description").and_then(|v| v.as_str()) {
                            desc.to_string()
                        } else if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
                            cmd.to_string()
                        } else if let Some(query) = input.get("query").and_then(|v| v.as_str()) {
                            query.to_string()
                        } else if let Some(path) = input
                            .get("path")
                            .or_else(|| input.get("file_path"))
                            .and_then(|v| v.as_str())
                        {
                            path.to_string()
                        } else if let Some(url) = input.get("url").and_then(|v| v.as_str()) {
                            url.to_string()
                        } else {
                            tool_name.to_string()
                        }
                    } else {
                        tool_name.to_string()
                    };

                    let is_approval_tool = tool_name == "AskFollowupQuestion"
                        || tool_name == "ExitPlanMode"
                        || tool_name.to_ascii_lowercase().contains("confirm")
                        || tool_name.to_ascii_lowercase().contains("permission");

                    let step_progress_str = if total_tools > 1 {
                        format!(" [1/{}]", total_tools)
                    } else {
                        String::new()
                    };

                    let (state, status_text) = if is_approval_tool {
                        (
                            AgentState::WaitingForApproval {
                                name: tool_name.to_string(),
                                summary: tool_summary.clone(),
                            },
                            format!(
                                "PERMISSION REQUIRED{}: {} - {}",
                                step_progress_str, tool_name, tool_summary
                            ),
                        )
                    } else {
                        (
                            AgentState::RunningTool {
                                name: tool_name.to_string(),
                                summary: tool_summary.clone(),
                            },
                            format!(
                                "RUNNING TOOL{}: {} - {}",
                                step_progress_str, tool_name, tool_summary
                            ),
                        )
                    };

                    return Some(ParsedClaudeStep {
                        session_id,
                        cwd,
                        step_index,
                        state,
                        status_text,
                        prompt_preview: None,
                    });
                }
            }

            if stop_reason == "end_turn" {
                return Some(ParsedClaudeStep {
                    session_id,
                    cwd,
                    step_index,
                    state: AgentState::WaitingForInput {
                        prompt_preview: "Ready for input".to_string(),
                    },
                    status_text: "WAITING FOR PROMPT".to_string(),
                    prompt_preview: None,
                });
            }

            return Some(ParsedClaudeStep {
                session_id,
                cwd,
                step_index,
                state: AgentState::Thinking,
                status_text: "THINKING • REASONING...".to_string(),
                prompt_preview: None,
            });
        }

        // 2. User message / tool result
        if role == "user" {
            let content_blocks = message
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array());

            if let Some(blocks) = content_blocks {
                let has_tool_result = blocks
                    .iter()
                    .any(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_result"));
                if has_tool_result {
                    let denial_detected = blocks.iter().any(|b| {
                        let text = b.to_string();
                        text.contains("Permission denied")
                            || text.contains("permission denied")
                            || text.contains("User rejected")
                            || text.contains("rejected by user")
                            || text.contains("Tool call rejected")
                            || text.contains("declined by user")
                    });

                    if denial_detected {
                        return Some(ParsedClaudeStep {
                            session_id,
                            cwd,
                            step_index,
                            state: AgentState::WaitingForInput {
                                prompt_preview: "Permission denied - ready for next instruction"
                                    .to_string(),
                            },
                            status_text: "PERMISSION DENIED • WAITING FOR PROMPT".to_string(),
                            prompt_preview: None,
                        });
                    }

                    return Some(ParsedClaudeStep {
                        session_id,
                        cwd,
                        step_index,
                        state: AgentState::Thinking,
                        status_text: "PROCESSING TOOL RESULT".to_string(),
                        prompt_preview: None,
                    });
                }
            }

            let user_text = if let Some(content_str) =
                message.and_then(|m| m.get("content")).and_then(|c| c.as_str())
            {
                content_str.to_string()
            } else if let Some(blocks) = content_blocks {
                blocks
                    .iter()
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                String::new()
            };

            let clean_user_text = user_text
                .lines()
                .filter(|l| !l.starts_with('<') && !l.ends_with('>'))
                .collect::<Vec<_>>()
                .join(" ");
            let preview: String = clean_user_text.trim().chars().take(60).collect();

            let prompt_preview = if !clean_user_text.trim().is_empty() {
                Some(clean_user_text.trim().to_string())
            } else {
                None
            };

            return Some(ParsedClaudeStep {
                session_id,
                cwd,
                step_index,
                state: AgentState::Thinking,
                status_text: if preview.is_empty() {
                    "PROCESSING PROMPT".to_string()
                } else {
                    format!("PROCESSING: {}", preview)
                },
                prompt_preview,
            });
        }

        // 3. Progress event
        if entry_type == "progress" {
            let query = json
                .get("data")
                .and_then(|d| d.get("query"))
                .and_then(|q| q.as_str())
                .unwrap_or("");
            return Some(ParsedClaudeStep {
                session_id,
                cwd,
                step_index,
                state: AgentState::RunningTool {
                    name: "progress".to_string(),
                    summary: query.to_string(),
                },
                status_text: if query.is_empty() {
                    "RUNNING TOOL PROGRESS".to_string()
                } else {
                    format!("RUNNING: {}", query)
                },
                prompt_preview: None,
            });
        }

        None
    }
}

/// Extracts the earliest markdown H1 heading from a directory.
pub fn extract_earliest_markdown_heading(session_dir: &Path) -> Option<String> {
    if let Ok(entries) = std::fs::read_dir(session_dir) {
        let mut md_files: Vec<(std::path::PathBuf, SystemTime)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && path
                    .extension()
                    .map_or(false, |ext| ext.eq_ignore_ascii_case("md"))
            {
                if let Ok(meta) = path.metadata() {
                    let created = meta
                        .created()
                        .or_else(|_| meta.modified())
                        .unwrap_or(SystemTime::UNIX_EPOCH);
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
                            .trim_start_matches("Teamwork Project Prompt — ")
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

/// Extracts working directory basename from transcript events.
pub fn extract_workdir_basename(transcript_path: &Path) -> Option<String> {
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
                            let name_clean =
                                name.trim().trim_matches(['\\', '/', ' ', '\r', '\n']);
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
                                if let Some(name) =
                                    Path::new(cwd).file_name().and_then(|n| n.to_str())
                                {
                                    if !name.trim().is_empty() {
                                        return Some(name.to_string());
                                    }
                                }
                            }
                            if let Some(search_path) =
                                args.get("SearchPath").and_then(|v| v.as_str())
                            {
                                if let Some(name) =
                                    Path::new(search_path).file_name().and_then(|n| n.to_str())
                                {
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

/// Fallback user prompt preview for display title.
pub fn extract_prompt_fallback(transcript_path: &Path) -> Option<String> {
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

/// Extracts title for Claude Code transcripts from first prompt or project folder.
pub fn extract_claude_title(transcript_path: &Path, project_dir: &Path) -> String {
    if let Ok(file) = File::open(transcript_path) {
        let reader = BufReader::new(file);
        for line in reader.lines().flatten().take(15) {
            if let Ok(json) = serde_json::from_str::<Value>(&line) {
                if let Some(parsed) = ClaudeParser::parse_line(&json, 0) {
                    if let Some(prompt) = parsed.prompt_preview {
                        let max_chars = 32;
                        let clean: String = prompt
                            .lines()
                            .map(|l| l.trim())
                            .filter(|l| !l.is_empty() && !l.starts_with('<'))
                            .collect::<Vec<_>>()
                            .join(" ");
                        let trimmed = clean.trim();
                        if !trimmed.is_empty() {
                            let char_count = trimmed.chars().count();
                            let mut result: String = trimmed.chars().take(max_chars).collect();
                            if char_count > max_chars {
                                result.push_str("..");
                            }
                            return result;
                        }
                    }
                }
            }
        }
    }

    // Fallback to project directory name clean decoding
    if let Some(folder_name) = project_dir.file_name().and_then(|n| n.to_str()) {
        let clean = folder_name.replace("C--Users-schordinger-workbench-", "");
        let clean = clean.replace("C--Users-schordinger-", "");
        let clean = clean.replace("home-schordinger-workbench-", "");
        let clean = clean.replace("home-schordinger-", "");
        if !clean.is_empty() && clean != folder_name {
            return clean;
        }
    }

    if let Some(file_stem) = transcript_path.file_stem().and_then(|s| s.to_str()) {
        format!("Claude {}", &file_stem[..6.min(file_stem.len())])
    } else {
        "Claude Session".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_safe_line_reader_full_lines() {
        let data = "{\"step\":1}\n{\"step\":2}\n{\"step\":3}\n";
        let mut cursor = Cursor::new(data.as_bytes());
        let file_len = data.len() as u64;

        let mut lines = Vec::new();
        let (new_offset, count) = SafeLineReader::read_new_lines(
            &mut cursor,
            file_len,
            0,
            8192,
            |s, _| {
                lines.push(s.to_string());
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(count, 3);
        assert_eq!(new_offset, file_len);
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn test_safe_line_reader_partial_line_truncation() {
        // Line 1 is complete, line 2 has no newline (partial write)
        let complete_part = "{\"step\":1}\n";
        let partial_part = "{\"step\":2, \"status\": \"RUN";
        let data = format!("{}{}", complete_part, partial_part);
        let mut cursor = Cursor::new(data.as_bytes());
        let file_len = data.len() as u64;

        let mut lines = Vec::new();
        let (new_offset, count) = SafeLineReader::read_new_lines(
            &mut cursor,
            file_len,
            0,
            8192,
            |s, _| {
                lines.push(s.to_string());
                Ok(())
            },
        )
        .unwrap();

        // Must ONLY advance to byte position after the last complete newline!
        assert_eq!(count, 1);
        assert_eq!(new_offset, complete_part.len() as u64);
        assert_eq!(lines, vec!["{\"step\":1}"]);

        // When the writer flushes the rest of line 2 + line 3:
        let completed_data = format!("{}{}{}", complete_part, "{\"step\":2, \"status\": \"RUNNING\"}\n", "{\"step\":3}\n");
        let mut cursor2 = Cursor::new(completed_data.as_bytes());
        let (resumed_offset, count2) = SafeLineReader::read_new_lines(
            &mut cursor2,
            completed_data.len() as u64,
            new_offset, // Resumes from exactly after line 1
            8192,
            |s, _| {
                lines.push(s.to_string());
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(count2, 2);
        assert_eq!(resumed_offset, completed_data.len() as u64);
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn test_safe_line_reader_tail_seeking_discards_incomplete_first_line() {
        let prefix_line = "{\"step\":9, \"content\": \"this line was truncated mid-seek\"}\n";
        let complete_lines = "{\"step\":10}\n{\"step\":11}\n";
        let data = format!("{}{}", prefix_line, complete_lines);
        let mut cursor = Cursor::new(data.as_bytes());
        let file_len = data.len() as u64;

        // Force tail seek into the middle of prefix_line
        let tail_limit = complete_lines.len() as u64 + 20;
        let mut lines = Vec::new();
        let (new_offset, count) = SafeLineReader::read_new_lines(
            &mut cursor,
            file_len,
            0,
            tail_limit,
            |s, _| {
                lines.push(s.to_string());
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(count, 2);
        assert_eq!(new_offset, file_len);
        assert_eq!(lines, vec!["{\"step\":10}", "{\"step\":11}"]);
    }

    #[test]
    fn test_safe_line_reader_corrupted_complete_line_is_skipped_and_advances() {
        let valid_line1 = "{\"step\":1}\n";
        let invalid_line = "THIS IS NOT VALID JSON\n";
        let valid_line2 = "{\"step\":2}\n";
        let data = format!("{}{}{}", valid_line1, invalid_line, valid_line2);
        let mut cursor = Cursor::new(data.as_bytes());
        let file_len = data.len() as u64;

        let mut lines = Vec::new();
        let (new_offset, count) = SafeLineReader::read_new_lines(
            &mut cursor,
            file_len,
            0,
            8192,
            |s, _| {
                lines.push(s.to_string());
                Ok(())
            },
        )
        .unwrap();

        // Should parse line 1, skip invalid complete line, and parse line 2
        assert_eq!(count, 2);
        assert_eq!(new_offset, file_len);
        assert_eq!(lines, vec!["{\"step\":1}", "{\"step\":2}"]);
    }

    #[test]
    fn test_antigravity_parser_running_tool_vs_waiting_approval() {
        // 1. Tool call requiring approval
        let approval_json = serde_json::json!({
            "step_index": 1,
            "type": "PLANNER_RESPONSE",
            "status": "WAITING_FOR_APPROVAL",
            "tool_calls": [{
                "name": "run_command",
                "args": {
                    "CommandLine": "cargo test",
                    "toolSummary": "Run integration tests",
                    "toolAction": "Running command"
                }
            }]
        });
        let parsed = AntigravityParser::parse_step(&approval_json).unwrap();
        assert!(matches!(parsed.state, AgentState::WaitingForApproval { .. }));
        assert!(parsed.status_text.contains("PERMISSION REQUIRED"));

        // 2. Tool call actively executing (RunningTool)
        let running_json = serde_json::json!({
            "step_index": 2,
            "type": "PLANNER_RESPONSE",
            "status": "RUNNING",
            "tool_calls": [{
                "name": "replace_file_content",
                "args": {
                    "toolSummary": "Update token lifetime",
                    "toolAction": "Editing file"
                }
            }]
        });
        let parsed2 = AntigravityParser::parse_step(&running_json).unwrap();
        assert!(matches!(parsed2.state, AgentState::RunningTool { .. }));
        assert!(parsed2.status_text.contains("RUNNING TOOL"));
    }

    #[test]
    fn test_antigravity_parser_multi_step_tool_calls() {
        let multi_tool_json = serde_json::json!({
            "step_index": 3,
            "type": "PLANNER_RESPONSE",
            "status": "RUNNING",
            "tool_calls": [
                { "name": "view_file", "args": { "toolSummary": "Inspect file 1" }, "status": "DONE" },
                { "name": "replace_file_content", "args": { "toolSummary": "Edit file 2", "toolAction": "Editing" }, "status": "RUNNING" },
                { "name": "run_command", "args": { "toolSummary": "Build" }, "status": "PENDING" }
            ]
        });
        let parsed = AntigravityParser::parse_step(&multi_tool_json).unwrap();
        assert!(matches!(parsed.state, AgentState::RunningTool { .. }));
        assert!(parsed.status_text.contains("[2/3]"));
    }

    #[test]
    fn test_antigravity_parser_permission_denial() {
        let denial_json = serde_json::json!({
            "step_index": 4,
            "type": "GENERIC",
            "status": "DENIED",
            "content": "User denied permission to run command"
        });
        let parsed = AntigravityParser::parse_step(&denial_json).unwrap();
        assert!(matches!(parsed.state, AgentState::WaitingForInput { .. }));
        assert!(parsed.status_text.contains("PERMISSION DENIED"));
    }

    #[test]
    fn test_antigravity_parser_aborted_turn() {
        let aborted_json = serde_json::json!({
            "step_index": 5,
            "type": "PLANNER_RESPONSE",
            "status": "ABORTED",
            "content": "Turn aborted"
        });
        let parsed = AntigravityParser::parse_step(&aborted_json).unwrap();
        assert!(matches!(parsed.state, AgentState::WaitingForInput { .. }));
        assert!(parsed.status_text.contains("ABORTED"));
    }

    #[test]
    fn test_claude_parser_tool_use_and_end_turn() {
        // 1. Tool use -> RunningTool
        let tool_use_json = serde_json::json!({
            "sessionId": "test-claude-123",
            "cwd": "C:\\Users\\schordinger\\workbench\\agent-deck",
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_01",
                    "name": "WebSearch",
                    "input": { "query": "Rust tokio broadcast error" }
                }],
                "stop_reason": "tool_use"
            }
        });
        let parsed = ClaudeParser::parse_line(&tool_use_json, 1).unwrap();
        assert!(matches!(parsed.state, AgentState::RunningTool { .. }));
        assert_eq!(parsed.session_id, Some("test-claude-123".to_string()));
        assert!(parsed.status_text.contains("RUNNING TOOL"));

        // 2. End turn -> WaitingForInput
        let end_turn_json = serde_json::json!({
            "sessionId": "test-claude-123",
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{ "type": "text", "text": "I have completed your task." }],
                "stop_reason": "end_turn"
            }
        });
        let parsed2 = ClaudeParser::parse_line(&end_turn_json, 2).unwrap();
        assert!(matches!(parsed2.state, AgentState::WaitingForInput { .. }));
        assert_eq!(parsed2.status_text, "WAITING FOR PROMPT");

        // 3. User permission denial in tool_result -> WaitingForInput
        let denial_result = serde_json::json!({
            "sessionId": "test-claude-123",
            "type": "user",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_01",
                    "content": "Permission denied by user"
                }]
            }
        });
        let parsed3 = ClaudeParser::parse_line(&denial_result, 3).unwrap();
        assert!(matches!(parsed3.state, AgentState::WaitingForInput { .. }));
        assert!(parsed3.status_text.contains("PERMISSION DENIED"));
    }

    #[test]
    fn test_antigravity_parser_direct_tool_steps() {
        let run_cmd_json = serde_json::json!({
            "step_index": 7,
            "type": "RUN_COMMAND",
            "status": "RUNNING",
            "content": "cargo test --workspace\nCompiling agent-deck..."
        });
        let parsed = AntigravityParser::parse_step(&run_cmd_json).unwrap();
        assert!(matches!(parsed.state, AgentState::RunningTool { .. }));
        assert_eq!(parsed.status_text, "RUNNING RUN_COMMAND: cargo test --workspace Compiling agent-deck...");
    }

    #[test]
    fn test_antigravity_parser_denial_variations() {
        let variations = vec![
            "User denied permission",
            "declined by user",
            "Tool call rejected by user",
            "Permission rejected",
        ];
        for text in variations {
            let json = serde_json::json!({
                "step_index": 8,
                "type": "GENERIC",
                "status": "DONE",
                "content": text
            });
            let parsed = AntigravityParser::parse_step(&json).unwrap();
            assert!(matches!(parsed.state, AgentState::WaitingForInput { .. }), "Failed for text: {}", text);
            assert!(parsed.status_text.contains("PERMISSION DENIED"));
        }
    }

    #[test]
    fn test_claude_parser_approval_tool_and_multi_tool() {
        // 1. Approval tool (AskFollowupQuestion)
        let ask_q_json = serde_json::json!({
            "sessionId": "test-claude-q",
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "name": "AskFollowupQuestion",
                    "input": { "question": "Should we proceed with migration?" }
                }]
            }
        });
        let parsed = ClaudeParser::parse_line(&ask_q_json, 1).unwrap();
        assert!(matches!(parsed.state, AgentState::WaitingForApproval { .. }));
        assert!(parsed.status_text.contains("PERMISSION REQUIRED"));

        // 2. Multi-tool assistant turn
        let multi_tool_json = serde_json::json!({
            "sessionId": "test-claude-multi",
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [
                    { "type": "tool_use", "name": "Read", "input": { "path": "src/lib.rs" } },
                    { "type": "tool_use", "name": "Write", "input": { "path": "src/lib.rs" } }
                ]
            }
        });
        let parsed2 = ClaudeParser::parse_line(&multi_tool_json, 2).unwrap();
        assert!(matches!(parsed2.state, AgentState::RunningTool { .. }));
        assert!(parsed2.status_text.contains("[1/2]"));
    }

    #[test]
    fn test_claude_parser_aborted_turn() {
        let aborted_json = serde_json::json!({
            "sessionId": "test-claude-abort",
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{ "type": "text", "text": "Starting analysis..." }],
                "stop_reason": "cancelled"
            }
        });
        let parsed = ClaudeParser::parse_line(&aborted_json, 1).unwrap();
        assert!(matches!(parsed.state, AgentState::WaitingForInput { .. }));
        assert!(parsed.status_text.contains("ABORTED"));
    }

    #[test]
    fn test_safe_line_reader_crlf_and_empty_lines() {
        let data = "{\"step\":1}\r\n\r\n{\"step\":2}\r\n";
        let mut cursor = std::io::Cursor::new(data.as_bytes());
        let file_len = data.len() as u64;

        let mut lines = Vec::new();
        let (new_offset, count) = SafeLineReader::read_new_lines(
            &mut cursor,
            file_len,
            0,
            8192,
            |s, _| {
                lines.push(s.to_string());
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(count, 2);
        assert_eq!(new_offset, file_len);
        assert_eq!(lines, vec!["{\"step\":1}", "{\"step\":2}"]);
    }
}
