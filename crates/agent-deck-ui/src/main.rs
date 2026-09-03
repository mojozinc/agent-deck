#![windows_subsystem = "windows"]

mod adapter;
mod hub;

use adapter::native_windows::NativeWindowsAdapter;
use adapter::wsl2_bridge::Wsl2BridgeAdapter;
use adapter::StreamAdapter;
use agent_deck_core::AgentState;
use eframe::egui;
use egui::{pos2, vec2, Color32, FontId, Rect, Rounding, Stroke};
use hub::{ActiveSession, CustomTitlesStorage, DynamicCategory, SessionHub, UserAction};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

fn setup_crash_logging() {
    let log_dir = if let Ok(appdata) = std::env::var("APPDATA") {
        std::path::PathBuf::from(appdata).join("agent-deck")
    } else {
        std::path::PathBuf::from(".")
    };
    let _ = std::fs::create_dir_all(&log_dir);
    let crash_log_path = log_dir.join("crash.log");
    let run_log_path = log_dir.join("agent-deck.log");

    let startup_msg = format!(
        "[{}] Agent Deck UI starting up (PID: {})\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
        std::process::id()
    );
    let _ = std::fs::write(&run_log_path, startup_msg);

    std::panic::set_hook(Box::new(move |panic_info| {
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let payload = panic_info.payload();
        let message = if let Some(s) = payload.downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "Unknown panic payload".to_string()
        };

        let location = panic_info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown location".to_string());

        let backtrace = std::backtrace::Backtrace::capture();

        let report = format!(
            "=======================================================\n\
             AGENT DECK CRASH REPORT - {}\n\
             =======================================================\n\
             Location: {}\n\
             Message:  {}\n\
             \n\
             Backtrace:\n\
             {}\n\
             =======================================================\n\n",
            timestamp, location, message, backtrace
        );

        let _ = std::fs::write(&crash_log_path, &report);
        let _ = std::fs::write("agent-deck-crash.log", &report);
    }));
}

pub struct AgentDeckApp {
    hub: SessionHub,
    selected_session_id: Option<String>,
    editing_session_id: Option<String>,
    edit_text_buffer: String,
    last_frame_time: Instant,
    pulse_phase: f32,
    is_compact_mode: bool,
    font_scale: f32,
}

impl AgentDeckApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.window_fill = Color32::from_rgb(16, 18, 22);
        visuals.panel_fill = Color32::from_rgb(16, 18, 22);
        cc.egui_ctx.set_visuals(visuals);

        let custom_titles = Arc::new(RwLock::new(CustomTitlesStorage::load()));
        let hub = SessionHub::new(custom_titles);

        // 1. In-Process Native Windows Watcher (monitors all active Windows sessions)
        let mut native_adapter = NativeWindowsAdapter::new();
        native_adapter.start(hub.sender());

        // 2. WSL2 Activity Bridge (Connecting to WSL2 daemon on 127.0.0.1:8765)
        let mut wsl2_adapter = Wsl2BridgeAdapter::new("127.0.0.1:8765");
        wsl2_adapter.start(hub.sender());

        Self {
            hub,
            selected_session_id: None,
            editing_session_id: None,
            edit_text_buffer: String::new(),
            last_frame_time: Instant::now(),
            pulse_phase: 0.0,
            is_compact_mode: false,
            font_scale: 1.15,
        }
    }

    fn render_session_row(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut ActiveSession,
        dt: f32,
        pulse_phase: f32,
        actions: &mut Vec<UserAction>,
    ) {
        let scale = self.font_scale;
        let is_active = matches!(session.state, AgentState::Thinking | AgentState::RunningTool { .. });
        let is_waiting_input = matches!(session.state, AgentState::WaitingForInput { .. });
        let is_waiting_approval = matches!(session.state, AgentState::WaitingForApproval { .. });
        let is_stale = session.is_stale();
        let should_pulse = session.attention.is_pulsating(&session.state);
        let is_editing = self.editing_session_id.as_deref() == Some(&session.session_id);

        // Update session's individual VU meter levels
        for (i, bar) in session.vu_levels.iter_mut().enumerate() {
            if is_stale {
                *bar = lerp(*bar, 0.0, dt * 6.0);
            } else if is_active {
                let wave = ((pulse_phase * 2.8 + i as f32 * 0.6).sin() * 0.5 + 0.5)
                    * ((pulse_phase * 1.1 + (8 - i) as f32 * 0.4).cos() * 0.4 + 0.6);
                *bar = lerp(*bar, wave, dt * 12.0);
            } else if is_waiting_input || is_waiting_approval {
                *bar = lerp(*bar, 0.0, dt * 6.0);
            } else {
                *bar = lerp(*bar, 0.05, dt * 4.0);
            }
        }

        // Marquee scroll only when actively running/thinking
        if is_active && !is_stale {
            session.marquee_offset += dt * 38.0;
        } else {
            session.marquee_offset = 0.0;
        }

        let row_height = if is_editing { 74.0 * scale.min(1.3) } else { 52.0 * scale.min(1.3) };
        let row_rect = ui.allocate_space(vec2(ui.available_width(), row_height)).1;

        let is_selected = self.selected_session_id.as_deref() == Some(&session.session_id);

        // Interact sense (click row to select & acknowledge alert)
        let response = ui.interact(row_rect, ui.id().with(&session.session_id), egui::Sense::click());
        if response.clicked() {
            self.selected_session_id = Some(session.session_id.clone());
            actions.push(UserAction::Select(session.session_id.clone()));
        }

        let painter = ui.painter_at(row_rect);

        // Draw Row Bezel & Background
        let bg_color = if is_selected {
            Color32::from_rgb(10, 22, 16)
        } else if response.hovered() {
            Color32::from_rgb(12, 18, 14)
        } else if is_stale {
            Color32::from_rgb(6, 9, 8)
        } else {
            Color32::from_rgb(7, 12, 9)
        };

        let stroke_color = if is_selected {
            Color32::from_rgb(0, 220, 140)
        } else if should_pulse {
            let breathe = (pulse_phase * 1.5).sin() * 0.5 + 0.5;
            if is_waiting_approval {
                Color32::from_rgb(
                    lerp(180.0, 255.0, breathe) as u8,
                    lerp(120.0, 180.0, breathe) as u8,
                    lerp(20.0, 40.0, breathe) as u8,
                )
            } else {
                Color32::from_rgb(
                    lerp(140.0, 255.0, breathe) as u8,
                    lerp(100.0, 205.0, breathe) as u8,
                    lerp(15.0, 25.0, breathe) as u8,
                )
            }
        } else if is_stale {
            Color32::from_rgb(45, 40, 35)
        } else if is_waiting_approval {
            Color32::from_rgb(255, 160, 30)
        } else if is_waiting_input {
            Color32::from_rgb(180, 140, 20)
        } else if response.hovered() {
            Color32::from_rgb(45, 75, 55)
        } else {
            Color32::from_rgb(26, 38, 30)
        };

        painter.rect_filled(row_rect, Rounding::same(4.0), bg_color);
        painter.rect_stroke(row_rect, Rounding::same(4.0), Stroke::new(1.2_f32, stroke_color));

        // Glass Scanline pattern
        let grid_color = Color32::from_rgba_unmultiplied(20, 45, 25, 30);
        for y in (row_rect.min.y as i32..row_rect.max.y as i32).step_by(3) {
            painter.line_segment(
                [pos2(row_rect.min.x, y as f32), pos2(row_rect.max.x, y as f32)],
                Stroke::new(0.5_f32, grid_color),
            );
        }

        // 1. Status LED Indicator on Left
        let (state_label, main_glow_color) = if is_stale {
            ("STALE", Color32::from_rgb(160, 135, 100))
        } else {
            match &session.state {
                AgentState::Thinking => ("THINKING", Color32::from_rgb(0, 255, 128)),
                AgentState::RunningTool { name, .. } => (name.as_str(), Color32::from_rgb(50, 255, 100)),
                AgentState::WaitingForApproval { .. } => ("APPROVAL REQUIRED", Color32::from_rgb(255, 160, 30)),
                AgentState::WaitingForInput { .. } => ("WAITING FOR PROMPT", Color32::from_rgb(255, 205, 20)),
                AgentState::Error { .. } => ("ERROR", Color32::from_rgb(255, 70, 70)),
                AgentState::Finished => ("FINISHED", Color32::from_rgb(0, 220, 255)),
                AgentState::Idle => ("IDLE", Color32::from_rgb(90, 130, 110)),
            }
        };

        let led_center = row_rect.min + vec2(12.0, 14.0);
        let pulse_intensity = if should_pulse {
            let breathe = (pulse_phase * 1.5).sin() * 0.35 + 0.65;
            breathe.clamp(0.2, 1.0)
        } else if is_stale {
            0.4
        } else if is_waiting_approval || is_waiting_input {
            0.85
        } else if is_active {
            ((pulse_phase * 2.2).sin() * 0.25 + 0.75).clamp(0.2, 1.0)
        } else {
            0.6
        };

        let glow_rgba = Color32::from_rgba_unmultiplied(
            (main_glow_color.r() as f32 * pulse_intensity) as u8,
            (main_glow_color.g() as f32 * pulse_intensity) as u8,
            (main_glow_color.b() as f32 * pulse_intensity) as u8,
            230,
        );
        painter.circle_filled(led_center, 4.5, glow_rgba);
        painter.circle_filled(led_center, 2.0, Color32::WHITE);

        // 2. Line 1: Clean Metadata & Badges (with optional WSL distro tag)
        let header_y = row_rect.min.y + 6.0;

        let host_tag = if let Some(distro) = session.metadata.host.strip_prefix("wsl:") {
            format!("{} • ", distro)
        } else {
            String::new()
        };

        let badge_text = if let Some(ref tmux_s) = session.metadata.tmux_session {
            if let Some(ref tmux_w) = session.metadata.tmux_window {
                format!("{} • {}tmux:{}:{}", session.agent_type, host_tag, tmux_s, tmux_w)
            } else {
                format!("{} • {}tmux:{}", session.agent_type, host_tag, tmux_s)
            }
        } else {
            if session.display_name.starts_with(&session.agent_type) {
                session.display_name.clone()
            } else {
                format!("{} • {}{}", session.agent_type, host_tag, session.display_name)
            }
        };

        let font_badge = FontId::monospace(10.5 * scale);
        let approx_char_w = 6.6 * scale;
        let badge_len_approx = badge_text.len() as f32 * approx_char_w;
        let badge_x = row_rect.min.x + 22.0;

        painter.text(
            pos2(badge_x, header_y),
            egui::Align2::LEFT_TOP,
            &badge_text,
            font_badge,
            Color32::from_rgb(0, 220, 200),
        );

        // Edit button [EDIT]
        let edit_btn_x = badge_x + badge_len_approx + 6.0;
        let edit_btn_rect = Rect::from_min_size(pos2(edit_btn_x, header_y), vec2(36.0 * scale, 13.0 * scale));
        let edit_btn_resp = ui.interact(edit_btn_rect, ui.id().with(&session.session_id).with("edit_btn"), egui::Sense::click());
        
        let edit_btn_col = if edit_btn_resp.hovered() {
            Color32::from_rgb(255, 220, 100)
        } else {
            Color32::from_rgb(70, 105, 90)
        };
        painter.text(edit_btn_rect.min, egui::Align2::LEFT_TOP, "[EDIT]", FontId::monospace(9.0 * scale), edit_btn_col);

        if edit_btn_resp.clicked() {
            self.editing_session_id = Some(session.session_id.clone());
            self.edit_text_buffer = session.display_name.clone();
        }

        let mut next_x = edit_btn_x + 42.0 * scale;

        // If stale, render prominent [DISMISS] button
        if is_stale {
            let dismiss_pill_rect = Rect::from_min_size(pos2(next_x, header_y), vec2(56.0 * scale, 13.0 * scale));
            let dismiss_pill_resp = ui.interact(dismiss_pill_rect, ui.id().with(&session.session_id).with("dismiss_pill"), egui::Sense::click());
            let pill_col = if dismiss_pill_resp.hovered() {
                Color32::from_rgb(255, 120, 120)
            } else {
                Color32::from_rgb(210, 140, 90)
            };
            painter.text(dismiss_pill_rect.min, egui::Align2::LEFT_TOP, "[DISMISS]", FontId::monospace(9.0 * scale), pill_col);
            if dismiss_pill_resp.clicked() {
                actions.push(UserAction::Dismiss(session.session_id.clone()));
            }
            next_x += 60.0 * scale;
        }

        let state_x = (next_x + 6.0).min(row_rect.max.x - 190.0);
        if state_x > next_x + 4.0 {
            painter.text(
                pos2(state_x, header_y),
                egui::Align2::LEFT_TOP,
                format!("• {}", state_label.to_uppercase()),
                FontId::monospace(9.5 * scale),
                main_glow_color,
            );
        }

        // Step Counter (Safely positioned with zero overlap)
        painter.text(
            pos2(row_rect.max.x - 84.0, header_y),
            egui::Align2::RIGHT_TOP,
            format!("STEP {:03}", session.step_count),
            FontId::monospace(9.0 * scale),
            Color32::from_rgb(60, 160, 90),
        );

        // Quick Close 'x' button on far right of row
        let close_btn_rect = Rect::from_min_size(pos2(row_rect.max.x - 76.0, header_y - 1.0), vec2(14.0 * scale, 13.0 * scale));
        let close_btn_resp = ui.interact(close_btn_rect, ui.id().with(&session.session_id).with("close_btn"), egui::Sense::click());
        let close_col = if close_btn_resp.hovered() {
            Color32::from_rgb(255, 100, 100)
        } else {
            Color32::from_rgb(65, 80, 75)
        };
        painter.text(close_btn_rect.min, egui::Align2::LEFT_TOP, "✕", FontId::monospace(9.0 * scale), close_col);
        if close_btn_resp.clicked() {
            actions.push(UserAction::Dismiss(session.session_id.clone()));
        }

        // 3. Line 2: Status Text Display
        let marquee_y = row_rect.min.y + 25.0 * scale.min(1.2);
        let marquee_area = Rect::from_min_max(
            pos2(row_rect.min.x + 8.0, marquee_y),
            pos2(row_rect.max.x - 68.0, marquee_y + 18.0 * scale),
        );

        let text_color = if is_stale {
            Color32::from_rgb(160, 150, 130)
        } else if is_waiting_approval {
            Color32::from_rgb(255, 180, 60)
        } else if is_waiting_input {
            Color32::from_rgb(255, 215, 60)
        } else {
            match &session.state {
                AgentState::Error { .. } => Color32::from_rgb(255, 120, 120),
                AgentState::Finished => Color32::from_rgb(100, 220, 255),
                _ => Color32::from_rgb(40, 255, 120),
            }
        };

        let font_status = FontId::monospace(11.5 * scale);

        let mut row_painter = ui.painter_at(row_rect);
        let prev_clip = row_painter.clip_rect();
        row_painter.set_clip_rect(marquee_area);

        if is_waiting_input || is_waiting_approval || is_stale {
            let status_line = if is_stale {
                format!("(Inactive > 15m) {}", session.status_text)
            } else {
                session.status_text.clone()
            };

            row_painter.text(
                pos2(marquee_area.min.x + 2.0, marquee_y),
                egui::Align2::LEFT_TOP,
                &status_line,
                font_status,
                text_color,
            );
        } else {
            let display_text = format!("   {}   ", session.status_text);
            let char_w = 7.0 * scale;
            let total_text_width = display_text.len() as f32 * char_w;
            let offset_mod = session.marquee_offset % (total_text_width + 40.0);
            let start_x = marquee_area.max.x - offset_mod;

            row_painter.text(pos2(start_x, marquee_y), egui::Align2::LEFT_TOP, &display_text, font_status.clone(), text_color);
            row_painter.text(pos2(start_x + total_text_width + 40.0, marquee_y), egui::Align2::LEFT_TOP, &display_text, font_status, text_color);
        }
        row_painter.set_clip_rect(prev_clip);

        // 4. Mini VU Meter on Right
        let vu_box_min = pos2(row_rect.max.x - 58.0, row_rect.min.y + 12.0);
        let num_bars = 6;
        let bar_w = 5.0;
        let bar_gap = 2.0;

        for i in 0..num_bars {
            let x = vu_box_min.x + i as f32 * (bar_w + bar_gap);
            let level = session.vu_levels[i % session.vu_levels.len()];
            let total_segments = 5;
            let active_segments = (level * total_segments as f32).round() as usize;

            for seg in 0..total_segments {
                let seg_y = (row_rect.min.y + 38.0) - (seg as f32 * 3.5);
                let seg_rect = Rect::from_min_size(pos2(x, seg_y), vec2(bar_w, 2.5));
                let seg_color = if seg < active_segments {
                    if seg >= 4 {
                        Color32::from_rgb(255, 80, 80) // Red Peak
                    } else if seg >= 3 {
                        Color32::from_rgb(255, 200, 30) // Amber Mid
                    } else {
                        Color32::from_rgb(0, 255, 100) // Green
                    }
                } else {
                    Color32::from_rgb(14, 24, 18)
                };
                painter.rect_filled(seg_rect, Rounding::ZERO, seg_color);
            }
        }

        // 5. Inline Rename Overlay
        if is_editing {
            let edit_y = row_rect.min.y + 46.0 * scale.min(1.2);
            let edit_ui_rect = Rect::from_min_size(pos2(row_rect.min.x + 8.0, edit_y), vec2(row_rect.width() - 16.0, 22.0));
            
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(edit_ui_rect), |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(Color32::from_rgb(0, 220, 200), egui::RichText::new("NAME:").monospace().size(9.5 * scale));
                    
                    let text_input = ui.add(
                        egui::TextEdit::singleline(&mut self.edit_text_buffer)
                            .desired_width(180.0 * scale)
                            .font(FontId::monospace(10.5 * scale))
                    );

                    let enter_pressed = text_input.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let save_clicked = ui.button(egui::RichText::new("Save").size(9.5 * scale)).clicked();
                    
                    if save_clicked || enter_pressed {
                        let new_name = self.edit_text_buffer.trim().to_string();
                        if !new_name.is_empty() {
                            session.display_name = new_name.clone();
                            actions.push(UserAction::Rename(session.session_id.clone(), new_name));
                        }
                        self.editing_session_id = None;
                    }

                    if ui.button(egui::RichText::new("Reset").size(9.5 * scale)).clicked() {
                        actions.push(UserAction::Rename(session.session_id.clone(), "".to_string()));
                        self.editing_session_id = None;
                    }

                    if ui.button(egui::RichText::new("Cancel").size(9.5 * scale)).clicked() {
                        self.editing_session_id = None;
                    }
                });
            });
        }
    }
}

impl eframe::App for AgentDeckApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(16));

        let now = Instant::now();
        let dt = now.duration_since(self.last_frame_time).as_secs_f32();
        self.last_frame_time = now;
        self.pulse_phase += dt * 4.0;
        let scale = self.font_scale;

        let mut frame_actions: Vec<UserAction> = Vec::new();

        // Clear all unacknowledged notification pulses whenever user clicks anywhere on the window
        if ctx.input(|i| i.pointer.any_click() || i.pointer.any_pressed()) {
            frame_actions.push(UserAction::AcknowledgeAll);
        }

        // Ingest stream updates
        self.hub.poll_events();

        // Dynamically compute active environment category tabs
        let active_categories = self.hub.active_categories();
        if self.hub.selected_tab_idx >= active_categories.len() {
            self.hub.selected_tab_idx = 0;
        }

        // Draw Retro Winamp Main Frame
        let panel_frame = egui::Frame::none()
            .fill(Color32::from_rgb(18, 20, 25))
            .stroke(Stroke::new(1.5_f32, Color32::from_rgb(60, 70, 84)))
            .rounding(Rounding::same(8.0))
            .inner_margin(egui::Margin::same(6.0));

        egui::CentralPanel::default().frame(panel_frame).show(ctx, |ui| {
            let full_rect = ui.max_rect();

            // Drag window from chassis
            let drag_response = ui.interact(full_rect, ui.id().with("deck_drag"), egui::Sense::drag());
            if drag_response.dragged() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }

            // Top Header: Clean Typography & Controls
            ui.horizontal(|ui| {
                ui.add_space(2.0);
                ui.painter().rect_filled(
                    Rect::from_min_size(ui.cursor().min + vec2(0.0, 2.0), vec2(14.0, 14.0)),
                    Rounding::same(2.0),
                    Color32::from_rgb(0, 210, 150),
                );
                ui.add_space(18.0);

                ui.colored_label(
                    Color32::from_rgb(200, 220, 245),
                    egui::RichText::new("AGENT-DECK v0.3").strong().size(12.0 * scale),
                );

                let active_bridges = self.hub.get_active_bridges();
                if !active_bridges.is_empty() {
                    let bridge_name = active_bridges.join(", ");
                    let is_recent_connect = self
                        .hub
                        .last_bridge_connected_at
                        .map(|t| t.elapsed().as_secs_f32() < 4.0)
                        .unwrap_or(false);

                    let link_col = if is_recent_connect {
                        let breathe = (self.pulse_phase * 1.5).sin() * 0.35 + 0.65;
                        Color32::from_rgb(0, (240.0 * breathe) as u8, (200.0 * breathe) as u8)
                    } else {
                        Color32::from_rgb(0, 210, 160)
                    };

                    ui.add_space(6.0);
                    ui.colored_label(
                        link_col,
                        egui::RichText::new(format!("● {} LINKED", bridge_name)).monospace().size(9.0 * scale),
                    );
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(egui::RichText::new("X").size(10.5 * scale).color(Color32::from_rgb(255, 100, 100))).clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui.button(egui::RichText::new(if self.is_compact_mode { "^" } else { "_" }).size(10.0 * scale)).clicked() {
                        self.is_compact_mode = !self.is_compact_mode;
                    }

                    // Font Zoom Controls (A+ / A-)
                    if ui.button(egui::RichText::new("A+").size(9.0 * scale)).clicked() {
                        self.font_scale = (self.font_scale + 0.08).min(1.6);
                    }
                    if ui.button(egui::RichText::new("A-").size(9.0 * scale)).clicked() {
                        self.font_scale = (self.font_scale - 0.08).max(0.85);
                    }
                });
            });

            ui.add_space(3.0);

            // Dynamic Category Tabs Rendering
            ui.horizontal(|ui| {
                for (tab_idx, cat) in active_categories.iter().enumerate() {
                    let matching_sessions = self.hub.sessions_for_category(cat);
                    let count = matching_sessions.len();
                    let is_unacked = SessionHub::has_unacknowledged_input(&matching_sessions);
                    let is_waiting = SessionHub::has_waiting_input(&matching_sessions);
                    let is_active = self.hub.selected_tab_idx == tab_idx;

                    let tab_bg = if is_active {
                        Color32::from_rgb(42, 52, 68)
                    } else {
                        Color32::from_rgb(24, 27, 34)
                    };

                    let tab_border = if is_unacked {
                        let breathe = (self.pulse_phase * 1.5).sin() * 0.5 + 0.5;
                        Color32::from_rgb(
                            lerp(140.0, 255.0, breathe) as u8,
                            lerp(100.0, 205.0, breathe) as u8,
                            0,
                        )
                    } else if is_waiting {
                        Color32::from_rgb(180, 140, 20)
                    } else if is_active {
                        Color32::from_rgb(0, 220, 160)
                    } else {
                        Color32::from_rgb(45, 52, 64)
                    };

                    let dot = if is_unacked {
                        "*"
                    } else if is_waiting {
                        "*"
                    } else {
                        "o"
                    };

                    let tab_label = format!("{} {} • {}", dot, cat.label, count);

                    let btn = egui::Button::new(
                        egui::RichText::new(tab_label)
                            .size(11.0 * scale)
                            .color(if is_active { Color32::WHITE } else { Color32::from_rgb(160, 175, 190) })
                    )
                    .fill(tab_bg)
                    .stroke(Stroke::new(1.0_f32, tab_border))
                    .rounding(Rounding::same(3.0));

                    if ui.add(btn).clicked() {
                        self.hub.selected_tab_idx = tab_idx;
                        frame_actions.push(UserAction::AcknowledgeCategory(cat.id.clone()));
                    }
                }
            });

            ui.add_space(4.0);

            if !self.is_compact_mode {
                let current_cat = active_categories.get(self.hub.selected_tab_idx).cloned().unwrap_or(active_categories[0].clone());
                let matching_ids: Vec<String> = self.hub.sessions_for_category(&current_cat).iter().map(|s| s.session_id.clone()).collect();

                let dt = dt;
                let pulse_phase = self.pulse_phase;
                let available_h = (ui.available_height() - 22.0).max(50.0);

                egui::ScrollArea::vertical()
                    .max_height(available_h)
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        if matching_ids.is_empty() {
                            ui.add_space(15.0);
                            ui.vertical_centered(|ui| {
                                ui.colored_label(
                                    Color32::from_rgb(110, 130, 145),
                                    egui::RichText::new("No active sessions in this environment").monospace().size(11.0 * scale),
                                );
                            });
                        } else {
                            for session_id in matching_ids {
                                if let Some(idx) = self.hub.sessions.iter().position(|s| s.session_id == session_id) {
                                    let mut session = self.hub.sessions[idx].clone();

                                    if let Ok(storage) = self.hub.custom_titles.read() {
                                        if let Some(custom) = storage.get_title(&session.session_id) {
                                            session.display_name = custom;
                                        }
                                    }

                                    self.render_session_row(ui, &mut session, dt, pulse_phase, &mut frame_actions);
                                    ui.add_space(3.0);
                                }
                            }
                        }
                    });

                ui.add_space(2.0);

                // Bottom Global Status Bar
                let total_sessions = self.hub.sessions.len();
                let total_waiting = self.hub.sessions.iter().filter(|s| matches!(s.state, AgentState::WaitingForInput { .. } | AgentState::WaitingForApproval { .. })).count();

                ui.horizontal(|ui| {
                    let status_msg = if total_waiting > 0 {
                        format!("* {} active • {} requiring input/approval", total_sessions, total_waiting)
                    } else {
                        format!("o {} active sessions monitored", total_sessions)
                    };

                    let msg_color = if total_waiting > 0 {
                        Color32::from_rgb(255, 205, 30)
                    } else {
                        Color32::from_rgb(60, 160, 95)
                    };

                    ui.colored_label(msg_color, egui::RichText::new(status_msg).monospace().size(9.5 * scale));

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Tactile Resize Handle in Bottom Right
                        let (resize_id, resize_rect) = ui.allocate_space(vec2(12.0, 12.0));
                        let resize_resp = ui.interact(resize_rect, resize_id, egui::Sense::drag());

                        let grip_color = if resize_resp.hovered() || resize_resp.dragged() {
                            Color32::from_rgb(0, 220, 160)
                        } else {
                            Color32::from_rgb(60, 75, 90)
                        };

                        let p = ui.painter();
                        p.line_segment([pos2(resize_rect.max.x - 2.0, resize_rect.max.y - 8.0), pos2(resize_rect.max.x - 8.0, resize_rect.max.y - 2.0)], Stroke::new(1.0_f32, grip_color));
                        p.line_segment([pos2(resize_rect.max.x - 2.0, resize_rect.max.y - 4.0), pos2(resize_rect.max.x - 4.0, resize_rect.max.y - 2.0)], Stroke::new(1.0_f32, grip_color));

                        if resize_resp.dragged() {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::BeginResize(egui::ResizeDirection::SouthEast));
                        }

                        ui.colored_label(
                            Color32::from_rgb(50, 75, 60),
                            egui::RichText::new("[EDIT] Rename • [DISMISS] Dismiss • Drag corner to resize").monospace().size(9.0 * scale),
                        );
                    });
                });
            }
        });

        // Pass 2: Apply all queued user actions cleanly outside render loops
        self.hub.apply_actions(frame_actions);
    }
}

fn main() -> eframe::Result<()> {
    setup_crash_logging();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([580.0, 260.0])
            .with_min_inner_size([460.0, 140.0])
            .with_max_inner_size([1200.0, 800.0])
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_resizable(true)
            .with_title("Agent Deck"),
        ..Default::default()
    };

    eframe::run_native(
        "Agent Deck",
        native_options,
        Box::new(|cc| Ok(Box::new(AgentDeckApp::new(cc)))),
    )
}
