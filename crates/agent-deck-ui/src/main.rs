#![windows_subsystem = "windows"]

mod adapter;
mod hub;

use adapter::mock::MockAdapter;
use adapter::native_windows::NativeWindowsAdapter;
use adapter::wsl2_bridge::Wsl2BridgeAdapter;
use adapter::StreamAdapter;
use agent_deck_core::AgentState;
use eframe::egui;
use egui::{pos2, vec2, Color32, FontId, Rect, Rounding, Stroke};
use hub::{ActiveSession, SessionHub, DEFAULT_TABS};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

pub struct AgentDeckApp {
    hub: SessionHub,
    selected_session_id: Option<String>,
    last_frame_time: Instant,
    pulse_phase: f32,
    sim_enabled: Arc<AtomicBool>,
    is_compact_mode: bool,
}

impl AgentDeckApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.window_fill = Color32::from_rgb(16, 18, 22);
        visuals.panel_fill = Color32::from_rgb(16, 18, 22);
        cc.egui_ctx.set_visuals(visuals);

        let mut hub = SessionHub::new();
        let sim_enabled = Arc::new(AtomicBool::new(false)); // Default SIM OFF

        // 1. In-Process Native Windows Watcher
        let mut native_adapter = NativeWindowsAdapter::new();
        native_adapter.start(hub.sender());

        // 2. WSL2 Activity Bridge (Connecting to WSL2 daemon on 127.0.0.1:8765)
        let mut wsl2_adapter = Wsl2BridgeAdapter::new("127.0.0.1:8765");
        wsl2_adapter.start(hub.sender());

        // 3. Mock Simulation Adapter
        let mut mock_adapter = MockAdapter::new(sim_enabled.clone());
        mock_adapter.start(hub.sender());

        Self {
            hub,
            selected_session_id: None,
            last_frame_time: Instant::now(),
            pulse_phase: 0.0,
            sim_enabled,
            is_compact_mode: false,
        }
    }

    fn render_session_row(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut ActiveSession,
        dt: f32,
        pulse_phase: f32,
    ) {
        let is_active = matches!(session.state, AgentState::Thinking | AgentState::RunningTool { .. });
        let is_waiting = matches!(session.state, AgentState::WaitingForInput { .. });
        let should_blink = session.attention.should_blink(&session.state);

        // Update session's individual VU meter levels
        for (i, bar) in session.vu_levels.iter_mut().enumerate() {
            if is_active {
                let wave = ((pulse_phase * 2.8 + i as f32 * 0.6).sin() * 0.5 + 0.5)
                    * ((pulse_phase * 1.1 + (8 - i) as f32 * 0.4).cos() * 0.4 + 0.6);
                *bar = lerp(*bar, wave, dt * 12.0);
            } else if is_waiting {
                let pulse = if should_blink {
                    (pulse_phase * 2.5).sin().abs() * 0.8
                } else {
                    0.5
                };
                *bar = lerp(*bar, pulse, dt * 8.0);
            } else {
                *bar = lerp(*bar, 0.05, dt * 4.0);
            }
        }

        // Marquee scroll
        session.marquee_offset += dt * 38.0;

        let row_height = 48.0;
        let row_rect = ui.allocate_space(vec2(ui.available_width(), row_height)).1;

        let is_selected = self.selected_session_id.as_deref() == Some(&session.session_id);

        // Interact sense (click row to select & acknowledge alert)
        let response = ui.interact(row_rect, ui.id().with(&session.session_id), egui::Sense::click());
        if response.clicked() {
            self.selected_session_id = Some(session.session_id.clone());
            session.attention.acknowledge();
        }

        let painter = ui.painter_at(row_rect);

        // Draw Row Bezel & Background
        let bg_color = if is_selected {
            Color32::from_rgb(10, 22, 16)
        } else if response.hovered() {
            Color32::from_rgb(12, 18, 14)
        } else {
            Color32::from_rgb(7, 12, 9)
        };

        let stroke_color = if is_selected {
            Color32::from_rgb(0, 220, 140)
        } else if should_blink {
            let blink = (pulse_phase * 2.5).sin() > 0.0;
            if blink {
                Color32::from_rgb(255, 190, 20)
            } else {
                Color32::from_rgb(110, 80, 10)
            }
        } else if is_waiting {
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
        let (state_label, main_glow_color) = match &session.state {
            AgentState::Thinking => ("THINKING", Color32::from_rgb(0, 255, 128)),
            AgentState::RunningTool { name, .. } => (name.as_str(), Color32::from_rgb(50, 255, 100)),
            AgentState::WaitingForInput { .. } => ("INPUT REQUIRED", Color32::from_rgb(255, 205, 20)),
            AgentState::Error { .. } => ("ERROR", Color32::from_rgb(255, 70, 70)),
            AgentState::Finished => ("FINISHED", Color32::from_rgb(0, 220, 255)),
            AgentState::Idle => ("IDLE", Color32::from_rgb(90, 130, 110)),
        };

        let led_center = row_rect.min + vec2(12.0, 14.0);
        let pulse_intensity = if should_blink {
            ((pulse_phase * 3.2).sin() * 0.4 + 0.6).clamp(0.1, 1.0)
        } else if is_waiting {
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
        painter.circle_filled(led_center, 4.2, glow_rgba);
        painter.circle_filled(led_center, 1.8, Color32::WHITE);

        // 2. Line 1: Clean Metadata & Badges
        let header_y = row_rect.min.y + 6.0;

        let badge_text = if let Some(ref tmux_s) = session.metadata.tmux_session {
            if let Some(ref tmux_w) = session.metadata.tmux_window {
                format!("tmux:{}:{}", tmux_s, tmux_w)
            } else {
                format!("tmux:{}", tmux_s)
            }
        } else {
            session.display_name.clone()
        };

        let badge_len_approx = badge_text.len() as f32 * 6.2;
        let badge_x = row_rect.min.x + 22.0;

        painter.text(
            pos2(badge_x, header_y),
            egui::Align2::LEFT_TOP,
            badge_text,
            FontId::monospace(9.5),
            Color32::from_rgb(0, 220, 200),
        );

        let state_x = (badge_x + badge_len_approx + 14.0).min(row_rect.max.x - 160.0);
        if state_x > badge_x + 30.0 {
            painter.text(
                pos2(state_x, header_y),
                egui::Align2::LEFT_TOP,
                format!("• {}", state_label.to_uppercase()),
                FontId::monospace(9.0),
                main_glow_color,
            );
        }

        painter.text(
            pos2(row_rect.max.x - 68.0, header_y),
            egui::Align2::RIGHT_TOP,
            format!("STEP {:03}", session.step_count),
            FontId::monospace(8.5),
            Color32::from_rgb(60, 160, 90),
        );

        // 3. Line 2: Status Marquee Ticker
        let marquee_y = row_rect.min.y + 24.0;
        let marquee_area = Rect::from_min_max(
            pos2(row_rect.min.x + 8.0, marquee_y),
            pos2(row_rect.max.x - 68.0, marquee_y + 16.0),
        );

        let display_text = format!("   {}   ", session.status_text);
        let text_color = match &session.state {
            AgentState::WaitingForInput { .. } => Color32::from_rgb(255, 225, 70),
            AgentState::Error { .. } => Color32::from_rgb(255, 120, 120),
            AgentState::Finished => Color32::from_rgb(100, 220, 255),
            _ => Color32::from_rgb(40, 255, 120),
        };

        let font = FontId::monospace(10.5);
        let approx_char_width = 6.4;
        let total_text_width = display_text.len() as f32 * approx_char_width;
        let offset_mod = session.marquee_offset % (total_text_width + 40.0);
        let start_x = marquee_area.max.x - offset_mod;

        let mut row_painter = ui.painter_at(row_rect);
        let prev_clip = row_painter.clip_rect();
        row_painter.set_clip_rect(marquee_area);
        row_painter.text(pos2(start_x, marquee_y), egui::Align2::LEFT_TOP, &display_text, font.clone(), text_color);
        row_painter.text(pos2(start_x + total_text_width + 40.0, marquee_y), egui::Align2::LEFT_TOP, &display_text, font, text_color);
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
                let seg_y = (row_rect.min.y + 36.0) - (seg as f32 * 3.5);
                let seg_rect = Rect::from_min_size(pos2(x, seg_y), vec2(bar_w, 2.5));
                let seg_color = if seg < active_segments {
                    if seg >= 4 {
                        Color32::from_rgb(255, 80, 80)
                    } else if seg >= 3 {
                        Color32::from_rgb(255, 200, 30)
                    } else {
                        Color32::from_rgb(0, 255, 100)
                    }
                } else {
                    Color32::from_rgb(14, 24, 18)
                };
                painter.rect_filled(seg_rect, Rounding::ZERO, seg_color);
            }
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

        // Ingest stream updates
        self.hub.poll_events();

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
                    egui::RichText::new("AGENT-DECK v0.3").strong().size(11.0),
                );

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(egui::RichText::new("✕").size(10.0).color(Color32::from_rgb(255, 100, 100))).clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui.button(egui::RichText::new(if self.is_compact_mode { "▲" } else { "▼" }).size(9.0)).clicked() {
                        self.is_compact_mode = !self.is_compact_mode;
                    }
                    let is_sim = self.sim_enabled.load(Ordering::Relaxed);
                    let mode_text = if is_sim { "SIM ACTIVE" } else { "SIM OFF" };
                    let mode_col = if is_sim { Color32::from_rgb(255, 190, 40) } else { Color32::from_rgb(120, 130, 145) };
                    if ui.button(egui::RichText::new(mode_text).size(9.5).color(mode_col)).clicked() {
                        self.sim_enabled.store(!is_sim, Ordering::Relaxed);
                    }
                });
            });

            ui.add_space(3.0);

            // Config-Driven Environment Tabs Rendering (Common generic loop)
            ui.horizontal(|ui| {
                for (tab_idx, tab_cfg) in DEFAULT_TABS.iter().enumerate() {
                    let matching_sessions = self.hub.sessions_matching(tab_cfg.filter);
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
                        let blink = (self.pulse_phase * 2.5).sin() > 0.0;
                        if blink { Color32::from_rgb(255, 205, 0) } else { Color32::from_rgb(120, 90, 0) }
                    } else if is_waiting {
                        Color32::from_rgb(180, 140, 20)
                    } else if is_active {
                        Color32::from_rgb(0, 220, 160)
                    } else {
                        Color32::from_rgb(45, 52, 64)
                    };

                    let dot = if is_unacked {
                        let blink = (self.pulse_phase * 2.5).sin() > 0.0;
                        if blink { "●" } else { "○" }
                    } else if is_waiting {
                        "●"
                    } else {
                        "○"
                    };

                    let tab_label = format!("{} {} • {}", dot, tab_cfg.label, count);

                    let btn = egui::Button::new(
                        egui::RichText::new(tab_label)
                            .size(10.0)
                            .color(if is_active { Color32::WHITE } else { Color32::from_rgb(160, 175, 190) })
                    )
                    .fill(tab_bg)
                    .stroke(Stroke::new(1.0_f32, tab_border))
                    .rounding(Rounding::same(3.0));

                    if ui.add(btn).clicked() {
                        self.hub.selected_tab_idx = tab_idx;
                        self.hub.acknowledge_matching(tab_cfg.filter);
                    }
                }
            });

            ui.add_space(4.0);

            if !self.is_compact_mode {
                // Reactive ScrollArea driven dynamically by selected TabConfig
                let current_tab_filter = DEFAULT_TABS
                    .get(self.hub.selected_tab_idx)
                    .map(|t| t.filter)
                    .unwrap_or(DEFAULT_TABS[0].filter);

                let dt = dt;
                let pulse_phase = self.pulse_phase;
                let available_h = (ui.available_height() - 22.0).max(50.0);

                egui::ScrollArea::vertical()
                    .max_height(available_h)
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        let mut matching_indices: Vec<usize> = Vec::new();
                        for (idx, s) in self.hub.sessions.iter().enumerate() {
                            if current_tab_filter(s) {
                                matching_indices.push(idx);
                            }
                        }

                        if matching_indices.is_empty() {
                            ui.add_space(15.0);
                            ui.vertical_centered(|ui| {
                                ui.colored_label(
                                    Color32::from_rgb(110, 130, 145),
                                    egui::RichText::new("No active sessions in this environment").monospace().size(10.0),
                                );
                            });
                        } else {
                            for idx in matching_indices {
                                let mut session = self.hub.sessions[idx].clone();
                                self.render_session_row(ui, &mut session, dt, pulse_phase);
                                self.hub.sessions[idx] = session;
                                ui.add_space(3.0);
                            }
                        }
                    });

                ui.add_space(2.0);

                // Bottom Global Status Bar
                let total_sessions = self.hub.sessions.len();
                let total_waiting = self.hub.sessions.iter().filter(|s| matches!(s.state, AgentState::WaitingForInput { .. })).count();

                ui.horizontal(|ui| {
                    let pulse_dot = if (self.pulse_phase * 1.5).sin() > 0.0 { "●" } else { "○" };
                    let status_msg = if total_waiting > 0 {
                        format!("{} {} active • {} requiring input", pulse_dot, total_sessions, total_waiting)
                    } else {
                        format!("{} {} active sessions monitored", pulse_dot, total_sessions)
                    };

                    let msg_color = if total_waiting > 0 {
                        Color32::from_rgb(255, 205, 30)
                    } else {
                        Color32::from_rgb(60, 160, 95)
                    };

                    ui.colored_label(msg_color, egui::RichText::new(status_msg).monospace().size(8.5));

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
                            egui::RichText::new("Click to Ack • Drag corner to resize").monospace().size(8.0),
                        );
                    });
                });
            }
        });
    }
}

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 240.0])
            .with_min_inner_size([440.0, 130.0])
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
