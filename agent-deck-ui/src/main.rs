use eframe::egui;
use egui::{pos2, vec2, Color32, FontId, Rect, Rounding, Stroke};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::thread;
use std::time::{Duration, Instant};

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

#[derive(Clone, Debug, PartialEq)]
pub enum AgentState {
    Idle,
    Thinking,
    RunningTool { name: String, summary: String },
    WaitingForInput { prompt_preview: String },
    Error { message: String },
    Finished,
}

#[derive(Clone, Debug)]
pub struct ChannelSession {
    pub name: String,
    pub agent_type: String, // "AGY", "Claude", "Codex"
    pub state: AgentState,
    pub status_text: String,
    pub step_count: u32,
    pub last_updated: Instant,
    pub is_live: bool,
}

pub struct AgentDeckApp {
    channels: Vec<ChannelSession>,
    active_channel_idx: usize,
    marquee_offset: f32,
    last_frame_time: Instant,
    vu_levels: [f32; 16],
    pulse_phase: f32,
    is_mock_mode: bool,
    mock_timer: Instant,
    mock_scenario_idx: usize,
    rx_live_updates: Option<Receiver<LiveUpdate>>,
    is_compact_mode: bool,
}

#[derive(Debug)]
pub struct LiveUpdate {
    pub channel_name: String,
    pub agent_type: String,
    pub state: AgentState,
    pub status_text: String,
    pub step_count: u32,
}

impl AgentDeckApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Setup custom dark visuals
        let mut visuals = egui::Visuals::dark();
        visuals.window_fill = Color32::from_rgb(18, 20, 24);
        visuals.panel_fill = Color32::from_rgb(18, 20, 24);
        cc.egui_ctx.set_visuals(visuals);

        // Start background watcher for native Windows AGY sessions (Stage 2)
        let (tx, rx) = channel::<LiveUpdate>();
        thread::spawn(move || {
            watch_native_agy_sessions(tx);
        });

        let channels = vec![
            ChannelSession {
                name: "AGY (Native Live)".to_string(),
                agent_type: "AGY".to_string(),
                state: AgentState::Idle,
                status_text: "SCANNING ANTIGRAVITY BRAIN...".to_string(),
                step_count: 0,
                last_updated: Instant::now(),
                is_live: true,
            },
            ChannelSession {
                name: "Claude Code".to_string(),
                agent_type: "Claude".to_string(),
                state: AgentState::RunningTool {
                    name: "grep_search".to_string(),
                    summary: "Searching AST parser rules".to_string(),
                },
                status_text: "GREP: Searching AST parser rules across 142 files...".to_string(),
                step_count: 18,
                last_updated: Instant::now(),
                is_live: false,
            },
            ChannelSession {
                name: "Codex CLI".to_string(),
                agent_type: "Codex".to_string(),
                state: AgentState::WaitingForInput {
                    prompt_preview: "Proceed with deleting temp migrations? [Y/n]".to_string(),
                },
                status_text: "WAITING FOR INPUT: Confirm file overwrite [Y/n]".to_string(),
                step_count: 42,
                last_updated: Instant::now(),
                is_live: false,
            },
        ];

        Self {
            channels,
            active_channel_idx: 0,
            marquee_offset: 0.0,
            last_frame_time: Instant::now(),
            vu_levels: [0.0; 16],
            pulse_phase: 0.0,
            is_mock_mode: false,
            mock_timer: Instant::now(),
            mock_scenario_idx: 0,
            rx_live_updates: Some(rx),
            is_compact_mode: false,
        }
    }

    fn update_mock_simulations(&mut self) {
        if !self.is_mock_mode {
            return;
        }

        if self.mock_timer.elapsed() > Duration::from_millis(3500) {
            self.mock_timer = Instant::now();
            self.mock_scenario_idx = (self.mock_scenario_idx + 1) % 5;

            // Update Channel 2 (Claude) and 3 (Codex) mock states
            match self.mock_scenario_idx {
                0 => {
                    self.channels[1].state = AgentState::Thinking;
                    self.channels[1].status_text = "THINKING: Analyzing call hierarchy for auth_middleware.rs".to_string();
                    self.channels[2].state = AgentState::RunningTool {
                        name: "cargo_test".to_string(),
                        summary: "Running integration tests".to_string(),
                    };
                    self.channels[2].status_text = "TEST: Running cargo test --workspace [14/18 passed]".to_string();
                }
                1 => {
                    self.channels[1].state = AgentState::RunningTool {
                        name: "replace_file_content".to_string(),
                        summary: "Patching JWT validation logic".to_string(),
                    };
                    self.channels[1].status_text = "EDIT: Patching JWT validation token TTL in session.rs".to_string();
                    self.channels[2].state = AgentState::WaitingForInput {
                        prompt_preview: "Allow execution of bash script? [y/N]".to_string(),
                    };
                    self.channels[2].status_text = "INPUT REQUIRED: Shell command permission pending".to_string();
                }
                2 => {
                    self.channels[1].state = AgentState::WaitingForInput {
                        prompt_preview: "Which database backend would you prefer?".to_string(),
                    };
                    self.channels[1].status_text = "INPUT REQUIRED: Select SQLite or PostgreSQL database".to_string();
                    self.channels[2].state = AgentState::Thinking;
                    self.channels[2].status_text = "THINKING: Synthesizing benchmark report...".to_string();
                }
                3 => {
                    self.channels[1].state = AgentState::Finished;
                    self.channels[1].status_text = "ALL TASKS COMPLETED: Branch ready for commit".to_string();
                    self.channels[2].state = AgentState::RunningTool {
                        name: "docker_build".to_string(),
                        summary: "Building container image".to_string(),
                    };
                    self.channels[2].status_text = "DOCKER: Building target release image [layer 6/9]".to_string();
                }
                _ => {
                    self.channels[1].state = AgentState::RunningTool {
                        name: "view_file".to_string(),
                        summary: "Reading config.toml".to_string(),
                    };
                    self.channels[1].status_text = "READING: Inspecting environment overrides in config.toml".to_string();
                    self.channels[2].state = AgentState::Idle;
                    self.channels[2].status_text = "CODEX IDLE: Listening for next prompt...".to_string();
                }
            }
        }
    }

    fn check_live_updates(&mut self) {
        if let Some(ref rx) = self.rx_live_updates {
            while let Ok(update) = rx.try_recv() {
                // Channel 0 is dedicated to Native AGY Live
                self.channels[0].state = update.state;
                self.channels[0].status_text = update.status_text;
                self.channels[0].step_count = update.step_count;
                self.channels[0].last_updated = Instant::now();
            }
        }
    }
}

impl eframe::App for AgentDeckApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Continuous repaint for smooth retro marquee & VU animations
        ctx.request_repaint_after(Duration::from_millis(16));

        let now = Instant::now();
        let dt = now.duration_since(self.last_frame_time).as_secs_f32();
        self.last_frame_time = now;
        self.pulse_phase += dt * 4.0;

        self.check_live_updates();
        self.update_mock_simulations();

        // Update animated VU meter bars based on active session state
        let current_state = &self.channels[self.active_channel_idx].state;
        let is_active = matches!(current_state, AgentState::Thinking | AgentState::RunningTool { .. });

        for (i, bar) in self.vu_levels.iter_mut().enumerate() {
            if is_active {
                let wave = ((self.pulse_phase * 2.5 + i as f32 * 0.45).sin() * 0.5 + 0.5)
                    * ((self.pulse_phase * 0.8 + (16 - i) as f32 * 0.3).cos() * 0.4 + 0.6);
                *bar = lerp(*bar, wave, dt * 10.0);
            } else if matches!(current_state, AgentState::WaitingForInput { .. }) {
                // Pulsing amber alert wave
                let pulse = (self.pulse_phase * 1.8).sin().abs() * 0.65;
                *bar = lerp(*bar, pulse, dt * 6.0);
            } else {
                *bar = lerp(*bar, 0.05, dt * 4.0);
            }
        }

        // Marquee scrolling
        self.marquee_offset += dt * 38.0;

        // Custom Deck Frame Styling
        let panel_frame = egui::Frame::none()
            .fill(Color32::from_rgb(22, 24, 29))
            .stroke(Stroke::new(1.5_f32, Color32::from_rgb(70, 78, 92)))
            .rounding(Rounding::same(8.0))
            .inner_margin(egui::Margin::same(6.0));

        egui::CentralPanel::default().frame(panel_frame).show(ctx, |ui| {
            let full_rect = ui.max_rect();

            // Window dragging from chassis
            let drag_response = ui.interact(full_rect, ui.id().with("chassis_drag"), egui::Sense::drag());
            if drag_response.dragged() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }

            // Top Header: Retro Winamp Title Bar & Window Controls
            ui.horizontal(|ui| {
                ui.add_space(2.0);
                // Beveled mini badge
                ui.painter().rect_filled(
                    Rect::from_min_size(ui.cursor().min + vec2(0.0, 2.0), vec2(14.0, 14.0)),
                    Rounding::same(2.0),
                    Color32::from_rgb(0, 190, 140),
                );
                ui.add_space(18.0);

                ui.colored_label(
                    Color32::from_rgb(200, 215, 235),
                    egui::RichText::new("CYBERAMP // AGENT-DECK v0.1").strong().size(11.0),
                );

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Close button
                    if ui.button(egui::RichText::new("✕").size(10.0).color(Color32::from_rgb(255, 100, 100))).clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    // Mini / Windowshade toggle
                    if ui.button(egui::RichText::new(if self.is_compact_mode { "▲" } else { "▼" }).size(9.0)).clicked() {
                        self.is_compact_mode = !self.is_compact_mode;
                    }
                    // Mock / Live Mode Toggle
                    let mode_btn_text = if self.is_mock_mode { "SIM: ON" } else { "SIM: OFF" };
                    let mode_btn_color = if self.is_mock_mode { Color32::from_rgb(255, 180, 50) } else { Color32::from_rgb(120, 130, 145) };
                    if ui.button(egui::RichText::new(mode_btn_text).size(9.5).color(mode_btn_color)).clicked() {
                        self.is_mock_mode = !self.is_mock_mode;
                    }
                });
            });

            ui.add_space(3.0);

            // Channel Selector Tabs (Winamp Preset Buttons)
            ui.horizontal(|ui| {
                for (i, ch) in self.channels.iter().enumerate() {
                    let is_active = self.active_channel_idx == i;
                    
                    // Status LED color for each channel
                    let led_color = match &ch.state {
                        AgentState::Thinking => Color32::from_rgb(0, 240, 120),
                        AgentState::RunningTool { .. } => Color32::from_rgb(0, 255, 100),
                        AgentState::WaitingForInput { .. } => {
                            let blink = (self.pulse_phase * 2.0).sin() > 0.0;
                            if blink {
                                Color32::from_rgb(255, 200, 0)
                            } else {
                                Color32::from_rgb(140, 100, 0)
                            }
                        }
                        AgentState::Error { .. } => Color32::from_rgb(255, 50, 50),
                        AgentState::Finished => Color32::from_rgb(0, 200, 255),
                        AgentState::Idle => Color32::from_rgb(70, 85, 105),
                    };

                    let btn_bg = if is_active {
                        Color32::from_rgb(45, 52, 65)
                    } else {
                        Color32::from_rgb(28, 31, 38)
                    };

                    let btn_text = egui::RichText::new(format!("● {}", ch.name))
                        .size(10.0)
                        .color(if is_active { Color32::WHITE } else { Color32::from_rgb(160, 175, 190) });

                    let button = egui::Button::new(btn_text)
                        .fill(btn_bg)
                        .stroke(Stroke::new(1.0_f32, if is_active { led_color } else { Color32::from_rgb(50, 56, 68) }))
                        .rounding(Rounding::same(3.0));

                    if ui.add(button).clicked() {
                        self.active_channel_idx = i;
                        self.marquee_offset = 0.0;
                    }
                }
            });

            ui.add_space(4.0);

            // Main Display: Skeuomorphic LCD Display + VU Meter
            let lcd_height = if self.is_compact_mode { 32.0 } else { 58.0 };
            let lcd_rect = ui.allocate_space(vec2(ui.available_width(), lcd_height)).1;
            
            // Draw LCD Bezel & Glass Background
            let mut painter = ui.painter_at(lcd_rect);
            painter.rect_filled(lcd_rect, Rounding::same(4.0), Color32::from_rgb(6, 12, 8));
            painter.rect_stroke(lcd_rect, Rounding::same(4.0), Stroke::new(1.5_f32, Color32::from_rgb(32, 48, 36)));

            // LCD Glass scanlines / grid pattern
            let grid_color = Color32::from_rgba_unmultiplied(20, 45, 25, 40);
            for y in (lcd_rect.min.y as i32..lcd_rect.max.y as i32).step_by(3) {
                painter.line_segment(
                    [pos2(lcd_rect.min.x, y as f32), pos2(lcd_rect.max.x, y as f32)],
                    Stroke::new(0.5_f32, grid_color),
                );
            }

            let active_ch = &self.channels[self.active_channel_idx];

            // 1. Channel Badge & State Pill (Top-Left of LCD)
            let (state_label, main_glow_color) = match &active_ch.state {
                AgentState::Thinking => ("THINKING", Color32::from_rgb(0, 255, 128)),
                AgentState::RunningTool { name, .. } => (name.as_str(), Color32::from_rgb(50, 255, 100)),
                AgentState::WaitingForInput { .. } => ("WAITING INPUT", Color32::from_rgb(255, 210, 20)),
                AgentState::Error { .. } => ("ERROR", Color32::from_rgb(255, 70, 70)),
                AgentState::Finished => ("FINISHED", Color32::from_rgb(0, 220, 255)),
                AgentState::Idle => ("IDLE / READY", Color32::from_rgb(90, 140, 110)),
            };

            // Glow LED on top-left
            let led_center = lcd_rect.min + vec2(12.0, 12.0);
            let pulse_intensity = ((self.pulse_phase * 2.5).sin() * 0.3 + 0.7).clamp(0.2, 1.0);
            let glow_rgba = Color32::from_rgba_unmultiplied(
                (main_glow_color.r() as f32 * pulse_intensity) as u8,
                (main_glow_color.g() as f32 * pulse_intensity) as u8,
                (main_glow_color.b() as f32 * pulse_intensity) as u8,
                220,
            );
            painter.circle_filled(led_center, 4.5, glow_rgba);
            painter.circle_filled(led_center, 2.0, Color32::WHITE);

            // Channel & State Text Header inside LCD
            painter.text(
                lcd_rect.min + vec2(22.0, 6.0),
                egui::Align2::LEFT_TOP,
                format!("[{}] {}", active_ch.agent_type, state_label.to_uppercase()),
                FontId::monospace(9.5),
                main_glow_color,
            );

            // Steps badge
            painter.text(
                pos2(lcd_rect.max.x - 8.0, lcd_rect.min.y + 6.0),
                egui::Align2::RIGHT_TOP,
                format!("STEP: #{:03}", active_ch.step_count),
                FontId::monospace(9.0),
                Color32::from_rgb(60, 160, 90),
            );

            // 2. Marquee Ticker: 1-Line Status Text
            let marquee_y = if self.is_compact_mode { lcd_rect.min.y + 17.0 } else { lcd_rect.min.y + 23.0 };
            let marquee_area = Rect::from_min_max(
                pos2(lcd_rect.min.x + 8.0, marquee_y),
                pos2(lcd_rect.max.x - 90.0, marquee_y + 14.0),
            );

            let display_text = format!("   *** {} ***   ", active_ch.status_text);
            let text_color = match &active_ch.state {
                AgentState::WaitingForInput { .. } => Color32::from_rgb(255, 230, 80),
                AgentState::Error { .. } => Color32::from_rgb(255, 120, 120),
                _ => Color32::from_rgb(40, 255, 120),
            };

            // Clip text to marquee area and render sliding position
            let font = FontId::monospace(11.0);
            let approx_char_width = 6.8;
            let total_text_width = display_text.len() as f32 * approx_char_width;
            let offset_mod = self.marquee_offset % (total_text_width + 40.0);
            let start_x = marquee_area.max.x - offset_mod;

            let prev_clip = painter.clip_rect();
            painter.set_clip_rect(marquee_area);
            painter.text(
                pos2(start_x, marquee_y),
                egui::Align2::LEFT_TOP,
                &display_text,
                font.clone(),
                text_color,
            );
            // Double draw to loop seamlessly
            painter.text(
                pos2(start_x + total_text_width + 40.0, marquee_y),
                egui::Align2::LEFT_TOP,
                &display_text,
                font,
                text_color,
            );
            painter.set_clip_rect(prev_clip);

            // 3. Mini VU Meter Bars (Right Side of LCD)
            let vu_box_min = pos2(lcd_rect.max.x - 82.0, marquee_y - 2.0);
            let num_bars = 10;
            let bar_w = 6.0;
            let bar_gap = 2.0;
            for i in 0..num_bars {
                let x = vu_box_min.x + i as f32 * (bar_w + bar_gap);
                let level = self.vu_levels[i % self.vu_levels.len()];
                let total_segments = 6;
                let active_segments = (level * total_segments as f32).round() as usize;

                for seg in 0..total_segments {
                    let seg_y = (marquee_y + 14.0) - (seg as f32 * 2.6);
                    let seg_rect = Rect::from_min_size(pos2(x, seg_y), vec2(bar_w, 2.0));
                    let seg_color = if seg < active_segments {
                        if seg >= 4 {
                            Color32::from_rgb(255, 80, 80) // Red peak
                        } else if seg >= 3 {
                            Color32::from_rgb(255, 200, 30) // Amber mid
                        } else {
                            Color32::from_rgb(0, 255, 100) // Green normal
                        }
                    } else {
                        Color32::from_rgb(16, 28, 20) // Unlit segment
                    };
                    painter.rect_filled(seg_rect, Rounding::ZERO, seg_color);
                }
            }

            // 4. Bottom Quick Status bar (Full mode only)
            if !self.is_compact_mode {
                let footer_y = lcd_rect.max.y - 14.0;
                let elapsed = active_ch.last_updated.elapsed().as_secs();
                let pulse_dot = if (self.pulse_phase * 1.5).sin() > 0.0 { "●" } else { "○" };
                painter.text(
                    pos2(lcd_rect.min.x + 8.0, footer_y),
                    egui::Align2::LEFT_TOP,
                    format!("{} LIVE FEED | UPDATED {}s AGO", pulse_dot, elapsed),
                    FontId::monospace(8.5),
                    Color32::from_rgb(45, 110, 70),
                );

                // Quick Hint
                painter.text(
                    pos2(lcd_rect.max.x - 8.0, footer_y),
                    egui::Align2::RIGHT_TOP,
                    "[DRAG TO MOVE | ALT+TAB]",
                    FontId::monospace(8.0),
                    Color32::from_rgb(45, 80, 60),
                );
            }
        });
    }
}

/// Stage 2: Background thread watching native Windows Antigravity (`agy`) session transcripts
fn watch_native_agy_sessions(tx: std::sync::mpsc::Sender<LiveUpdate>) {
    let home_dir = std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\Users\\schordinger".to_string());
    let brain_dir = PathBuf::from(home_dir).join(".gemini\\antigravity-cli\\brain");

    let mut current_watched_file: Option<PathBuf> = None;
    let mut last_file_pos: u64 = 0;

    loop {
        thread::sleep(Duration::from_millis(500));

        // 1. Find the latest modified session folder
        if let Ok(entries) = std::fs::read_dir(&brain_dir) {
            let mut latest_dir: Option<(PathBuf, std::time::SystemTime)> = None;
            for entry in entries.flatten() {
                if let Ok(file_type) = entry.file_type() {
                    if file_type.is_dir() {
                        let transcript_path = entry.path().join(".system_generated\\logs\\transcript.jsonl");
                        if transcript_path.exists() {
                            if let Ok(meta) = std::fs::metadata(&transcript_path) {
                                if let Ok(modified) = meta.modified() {
                                    if latest_dir.as_ref().map_or(true, |(_, latest_time)| modified > *latest_time) {
                                        latest_dir = Some((transcript_path, modified));
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if let Some((latest_transcript, _)) = latest_dir {
                // If switched to a newer session, reset read offset
                if current_watched_file.as_ref() != Some(&latest_transcript) {
                    current_watched_file = Some(latest_transcript.clone());
                    last_file_pos = 0;
                }

                // Read new lines from transcript.jsonl
                if let Ok(mut file) = File::open(&latest_transcript) {
                    let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
                    
                    // If file grew or we're starting up, read tail
                    if file_len > last_file_pos {
                        if last_file_pos == 0 && file_len > 8192 {
                            // On startup, jump to last 8KB to quickly get the latest status
                            let _ = file.seek(SeekFrom::Start(file_len - 8192));
                        } else {
                            let _ = file.seek(SeekFrom::Start(last_file_pos));
                        }

                        let reader = BufReader::new(file);
                        let mut last_valid_line = None;
                        for line in reader.lines().flatten() {
                            if !line.trim().is_empty() {
                                last_valid_line = Some(line);
                            }
                        }

                        last_file_pos = file_len;

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
                                            AgentState::RunningTool {
                                                name: tool_name.to_string(),
                                                summary: tool_summary.to_string(),
                                            },
                                            format!("TOOL [{}]: {} - {}", tool_name, tool_summary, tool_action),
                                        )
                                    } else {
                                        (AgentState::Thinking, "THINKING / REASONING...".to_string())
                                    }
                                } else if step_type == "USER_INPUT" || source == "USER_EXPLICIT" {
                                    let content = json.get("content").and_then(|v| v.as_str()).unwrap_or("");
                                    let preview: String = content.chars().take(60).collect();
                                    (
                                        AgentState::Thinking,
                                        format!("PROCESSING PROMPT: {}", preview),
                                    )
                                } else if step_type == "PLANNER_RESPONSE" && status == "DONE" {
                                    (
                                        AgentState::WaitingForInput {
                                            prompt_preview: "Ready for input".to_string(),
                                        },
                                        "WAITING FOR USER INPUT / PROMPT".to_string(),
                                    )
                                } else {
                                    (
                                        AgentState::Thinking,
                                        format!("STEP #{}: {}", step_index, step_type),
                                    )
                                };

                                let _ = tx.send(LiveUpdate {
                                    channel_name: "AGY (Native Live)".to_string(),
                                    agent_type: "AGY".to_string(),
                                    state,
                                    status_text,
                                    step_count: step_index,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
}

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([480.0, 115.0])
            .with_min_inner_size([380.0, 75.0])
            .with_max_inner_size([700.0, 160.0])
            .with_decorations(false) // Frameless retro floating look
            .with_transparent(true)
            .with_always_on_top() // Floating overlay
            .with_resizable(true)
            .with_title("CyberAmp // Agent Deck"),
        ..Default::default()
    };

    eframe::run_native(
        "CyberAmp Agent Deck",
        native_options,
        Box::new(|cc| Ok(Box::new(AgentDeckApp::new(cc)))),
    )
}

