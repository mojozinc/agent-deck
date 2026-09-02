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
use hub::SessionHub;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

pub struct AgentDeckApp {
    hub: SessionHub,
    active_channel_idx: usize,
    marquee_offset: f32,
    last_frame_time: Instant,
    vu_levels: [f32; 16],
    pulse_phase: f32,
    sim_enabled: Arc<AtomicBool>,
    is_compact_mode: bool,
}

impl AgentDeckApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.window_fill = Color32::from_rgb(18, 20, 24);
        visuals.panel_fill = Color32::from_rgb(18, 20, 24);
        cc.egui_ctx.set_visuals(visuals);

        let mut hub = SessionHub::new();
        let sim_enabled = Arc::new(AtomicBool::new(true));

        // 1. Start In-Process Native Windows Watcher Adapter
        let mut native_adapter = NativeWindowsAdapter::new();
        native_adapter.start(hub.sender());

        // 2. Start WSL2 Activity Bridge Adapter (TCP client to daemon on 127.0.0.1:8765)
        let mut wsl2_adapter = Wsl2BridgeAdapter::new("127.0.0.1:8765");
        wsl2_adapter.start(hub.sender());

        // 3. Start Mock Simulation Adapter (active by default for test mode)
        let mut mock_adapter = MockAdapter::new(sim_enabled.clone());
        mock_adapter.start(hub.sender());

        Self {
            hub,
            active_channel_idx: 0,
            marquee_offset: 0.0,
            last_frame_time: Instant::now(),
            vu_levels: [0.0; 16],
            pulse_phase: 0.0,
            sim_enabled,
            is_compact_mode: false,
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

        // Poll all stream adapters for live updates
        self.hub.poll_events();

        if self.active_channel_idx >= self.hub.sessions.len() {
            self.active_channel_idx = 0;
        }

        let current_state = &self.hub.sessions[self.active_channel_idx].state;
        let is_active = matches!(current_state, AgentState::Thinking | AgentState::RunningTool { .. });

        // Animate VU meter equalizer bars
        for (i, bar) in self.vu_levels.iter_mut().enumerate() {
            if is_active {
                let wave = ((self.pulse_phase * 2.5 + i as f32 * 0.45).sin() * 0.5 + 0.5)
                    * ((self.pulse_phase * 0.8 + (16 - i) as f32 * 0.3).cos() * 0.4 + 0.6);
                *bar = lerp(*bar, wave, dt * 10.0);
            } else if matches!(current_state, AgentState::WaitingForInput { .. }) {
                let pulse = (self.pulse_phase * 2.2).sin().abs() * 0.75;
                *bar = lerp(*bar, pulse, dt * 8.0);
            } else {
                *bar = lerp(*bar, 0.05, dt * 4.0);
            }
        }

        self.marquee_offset += dt * 40.0;

        // Draw Retro Winamp Chassis
        let panel_frame = egui::Frame::none()
            .fill(Color32::from_rgb(20, 22, 27))
            .stroke(Stroke::new(1.5_f32, Color32::from_rgb(65, 74, 88)))
            .rounding(Rounding::same(8.0))
            .inner_margin(egui::Margin::same(6.0));

        egui::CentralPanel::default().frame(panel_frame).show(ctx, |ui| {
            let full_rect = ui.max_rect();

            // Drag window from chassis
            let drag_response = ui.interact(full_rect, ui.id().with("chassis_drag"), egui::Sense::drag());
            if drag_response.dragged() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }

            // Top Header: Winamp Title & Controls
            ui.horizontal(|ui| {
                ui.add_space(2.0);
                ui.painter().rect_filled(
                    Rect::from_min_size(ui.cursor().min + vec2(0.0, 2.0), vec2(14.0, 14.0)),
                    Rounding::same(2.0),
                    Color32::from_rgb(0, 210, 150),
                );
                ui.add_space(18.0);

                ui.colored_label(
                    Color32::from_rgb(200, 220, 240),
                    egui::RichText::new("CYBERAMP // AGENT-DECK v0.2").strong().size(11.0),
                );

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(egui::RichText::new("✕").size(10.0).color(Color32::from_rgb(255, 100, 100))).clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui.button(egui::RichText::new(if self.is_compact_mode { "▲" } else { "▼" }).size(9.0)).clicked() {
                        self.is_compact_mode = !self.is_compact_mode;
                    }
                    let is_sim = self.sim_enabled.load(Ordering::Relaxed);
                    let mode_text = if is_sim { "SIM: ACTIVE" } else { "SIM: OFF" };
                    let mode_col = if is_sim { Color32::from_rgb(255, 190, 40) } else { Color32::from_rgb(120, 130, 145) };
                    if ui.button(egui::RichText::new(mode_text).size(9.5).color(mode_col)).clicked() {
                        self.sim_enabled.store(!is_sim, Ordering::Relaxed);
                    }
                });
            });

            ui.add_space(3.0);

            // Channel Selector Tabs (Dynamic Winamp Preset Buttons)
            ui.horizontal(|ui| {
                for (i, session) in self.hub.sessions.iter().enumerate() {
                    let is_active = self.active_channel_idx == i;

                    let led_color = match &session.state {
                        AgentState::Thinking => Color32::from_rgb(0, 240, 120),
                        AgentState::RunningTool { .. } => Color32::from_rgb(0, 255, 110),
                        AgentState::WaitingForInput { .. } => {
                            let blink = (self.pulse_phase * 2.2).sin() > 0.0;
                            if blink {
                                Color32::from_rgb(255, 205, 0)
                            } else {
                                Color32::from_rgb(130, 95, 0)
                            }
                        }
                        AgentState::Error { .. } => Color32::from_rgb(255, 50, 50),
                        AgentState::Finished => Color32::from_rgb(0, 210, 255),
                        AgentState::Idle => Color32::from_rgb(65, 80, 100),
                    };

                    let btn_bg = if is_active {
                        Color32::from_rgb(42, 50, 64)
                    } else {
                        Color32::from_rgb(26, 29, 36)
                    };

                    // Format badge with tmux session tag if available
                    let label_text = if let Some(ref tmux_s) = session.metadata.tmux_session {
                        format!("● [tmux:{}] {}", tmux_s, session.agent_type)
                    } else {
                        format!("● {}", session.display_name)
                    };

                    let btn_text = egui::RichText::new(label_text)
                        .size(9.5)
                        .color(if is_active { Color32::WHITE } else { Color32::from_rgb(155, 170, 185) });

                    let button = egui::Button::new(btn_text)
                        .fill(btn_bg)
                        .stroke(Stroke::new(1.0_f32, if is_active { led_color } else { Color32::from_rgb(48, 54, 66) }))
                        .rounding(Rounding::same(3.0));

                    if ui.add(button).clicked() {
                        self.active_channel_idx = i;
                        self.marquee_offset = 0.0;
                    }
                }
            });

            ui.add_space(4.0);

            // Main Display: Skeuomorphic LCD Display + Equalizer
            let lcd_height = if self.is_compact_mode { 32.0 } else { 58.0 };
            let lcd_rect = ui.allocate_space(vec2(ui.available_width(), lcd_height)).1;

            let mut painter = ui.painter_at(lcd_rect);
            painter.rect_filled(lcd_rect, Rounding::same(4.0), Color32::from_rgb(6, 12, 8));
            painter.rect_stroke(lcd_rect, Rounding::same(4.0), Stroke::new(1.5_f32, Color32::from_rgb(30, 46, 34)));

            // Scanlines
            let grid_color = Color32::from_rgba_unmultiplied(20, 45, 25, 40);
            for y in (lcd_rect.min.y as i32..lcd_rect.max.y as i32).step_by(3) {
                painter.line_segment(
                    [pos2(lcd_rect.min.x, y as f32), pos2(lcd_rect.max.x, y as f32)],
                    Stroke::new(0.5_f32, grid_color),
                );
            }

            let active_session = &self.hub.sessions[self.active_channel_idx];

            let (state_label, main_glow_color) = match &active_session.state {
                AgentState::Thinking => ("THINKING", Color32::from_rgb(0, 255, 128)),
                AgentState::RunningTool { name, .. } => (name.as_str(), Color32::from_rgb(50, 255, 100)),
                AgentState::WaitingForInput { .. } => ("WAITING INPUT", Color32::from_rgb(255, 210, 20)),
                AgentState::Error { .. } => ("ERROR", Color32::from_rgb(255, 70, 70)),
                AgentState::Finished => ("FINISHED", Color32::from_rgb(0, 220, 255)),
                AgentState::Idle => ("IDLE / READY", Color32::from_rgb(90, 140, 110)),
            };

            // Glow LED Indicator
            let led_center = lcd_rect.min + vec2(12.0, 12.0);
            let pulse_intensity = if matches!(active_session.state, AgentState::WaitingForInput { .. }) {
                ((self.pulse_phase * 3.0).sin() * 0.4 + 0.6).clamp(0.1, 1.0)
            } else {
                ((self.pulse_phase * 2.0).sin() * 0.25 + 0.75).clamp(0.2, 1.0)
            };

            let glow_rgba = Color32::from_rgba_unmultiplied(
                (main_glow_color.r() as f32 * pulse_intensity) as u8,
                (main_glow_color.g() as f32 * pulse_intensity) as u8,
                (main_glow_color.b() as f32 * pulse_intensity) as u8,
                230,
            );
            painter.circle_filled(led_center, 4.5, glow_rgba);
            painter.circle_filled(led_center, 2.0, Color32::WHITE);

            // Channel Header (with tmux info if present)
            let header_text = if let Some(ref tmux_s) = active_session.metadata.tmux_session {
                format!("[{}:{}] {}", active_session.agent_type, tmux_s, state_label.to_uppercase())
            } else {
                format!("[{}] {}", active_session.agent_type, state_label.to_uppercase())
            };

            painter.text(
                lcd_rect.min + vec2(22.0, 6.0),
                egui::Align2::LEFT_TOP,
                header_text,
                FontId::monospace(9.5),
                main_glow_color,
            );

            // Steps badge
            painter.text(
                pos2(lcd_rect.max.x - 8.0, lcd_rect.min.y + 6.0),
                egui::Align2::RIGHT_TOP,
                format!("STEP: #{:03}", active_session.step_count),
                FontId::monospace(9.0),
                Color32::from_rgb(60, 160, 90),
            );

            // Marquee Ticker
            let marquee_y = if self.is_compact_mode { lcd_rect.min.y + 17.0 } else { lcd_rect.min.y + 23.0 };
            let marquee_area = Rect::from_min_max(
                pos2(lcd_rect.min.x + 8.0, marquee_y),
                pos2(lcd_rect.max.x - 90.0, marquee_y + 14.0),
            );

            let display_text = format!("   *** {} ***   ", active_session.status_text);
            let text_color = match &active_session.state {
                AgentState::WaitingForInput { .. } => Color32::from_rgb(255, 230, 80),
                AgentState::Error { .. } => Color32::from_rgb(255, 120, 120),
                AgentState::Finished => Color32::from_rgb(100, 220, 255),
                _ => Color32::from_rgb(40, 255, 120),
            };

            let font = FontId::monospace(11.0);
            let approx_char_width = 6.8;
            let total_text_width = display_text.len() as f32 * approx_char_width;
            let offset_mod = self.marquee_offset % (total_text_width + 40.0);
            let start_x = marquee_area.max.x - offset_mod;

            let prev_clip = painter.clip_rect();
            painter.set_clip_rect(marquee_area);
            painter.text(pos2(start_x, marquee_y), egui::Align2::LEFT_TOP, &display_text, font.clone(), text_color);
            painter.text(pos2(start_x + total_text_width + 40.0, marquee_y), egui::Align2::LEFT_TOP, &display_text, font, text_color);
            painter.set_clip_rect(prev_clip);

            // VU Equalizer Bars
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
                            Color32::from_rgb(255, 80, 80)
                        } else if seg >= 3 {
                            Color32::from_rgb(255, 200, 30)
                        } else {
                            Color32::from_rgb(0, 255, 100)
                        }
                    } else {
                        Color32::from_rgb(16, 28, 20)
                    };
                    painter.rect_filled(seg_rect, Rounding::ZERO, seg_color);
                }
            }

            // Footer Status
            if !self.is_compact_mode {
                let footer_y = lcd_rect.max.y - 14.0;
                let elapsed = active_session.last_updated.elapsed().as_secs();
                let pulse_dot = if (self.pulse_phase * 1.5).sin() > 0.0 { "●" } else { "○" };
                painter.text(
                    pos2(lcd_rect.min.x + 8.0, footer_y),
                    egui::Align2::LEFT_TOP,
                    format!("{} HOST: {} | UPDATED {}s AGO", pulse_dot, active_session.metadata.host, elapsed),
                    FontId::monospace(8.5),
                    Color32::from_rgb(45, 110, 70),
                );

                painter.text(
                    pos2(lcd_rect.max.x - 8.0, footer_y),
                    egui::Align2::RIGHT_TOP,
                    "[DRAG TO MOVE | TAB TO SWITCH]",
                    FontId::monospace(8.0),
                    Color32::from_rgb(45, 80, 60),
                );
            }
        });
    }
}

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([540.0, 115.0])
            .with_min_inner_size([400.0, 75.0])
            .with_max_inner_size([850.0, 160.0])
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
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

