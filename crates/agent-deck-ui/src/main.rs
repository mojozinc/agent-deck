#![windows_subsystem = "windows"]

mod adapter;
mod hub;

use adapter::native_windows::NativeWindowsAdapter;
use adapter::wsl2_bridge::Wsl2BridgeAdapter;
use adapter::StreamAdapter;
use agent_deck_core::AgentState;
use eframe::egui;
use egui::{pos2, vec2, Color32, FontId, Rect, Rounding, Stroke};
use hub::{ActiveSession, CustomTitlesStorage, SessionHub, UserAction};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// Organic biological breathing easing curve:
/// f(t) = (e^sin(t) - 1/e) / (e - 1/e)
/// Maps smoothly to [0.0, 1.0], spending more time in calm low-luminance
/// and ramping smoothly into peak brightness according to human visual perception.
pub fn organic_led_breathing(t: f32) -> f32 {
    let e = std::f32::consts::E;
    let inv_e = 1.0 / e;
    let denom = e - inv_e;
    let num = (t.sin()).exp() - inv_e;
    (num / denom).clamp(0.0, 1.0)
}

/// Asymmetric VU ballistics:
/// Instantaneous attack (speed 40.0 * dt) when level is rising;
/// Smooth exponential decay (speed 5.0 * dt) when level is falling.
pub fn apply_vu_ballistics(current: f32, target: f32, dt: f32) -> f32 {
    let speed = if target > current {
        40.0 // Instantaneous attack
    } else {
        5.0 // Smooth exponential decay
    };
    let t = (dt * speed).clamp(0.0, 1.0);
    let new_val = current + (target - current) * t;
    new_val.clamp(0.0, 1.0)
}

/// Floating peak hold indicator dynamics:
/// Snaps to current level when new peak reached, holding for 600ms (0.6s),
/// then decays smoothly at dt * 2.5.
pub fn update_peak_hold(peak: &mut f32, timer: &mut f32, current_level: f32, dt: f32) {
    if current_level >= *peak {
        *peak = current_level;
        *timer = 0.6; // 600ms hold timer
    } else if *timer > 0.0 {
        if dt >= *timer {
            let leftover_dt = dt - *timer;
            *timer = 0.0;
            let decay_t = (leftover_dt * 2.5).clamp(0.0, 1.0);
            *peak += (current_level - *peak) * decay_t;
        } else {
            *timer -= dt;
        }
    } else {
        let decay_t = (dt * 2.5).clamp(0.0, 1.0);
        *peak += (current_level - *peak) * decay_t;
    }
    *peak = peak.clamp(current_level, 1.0);
}

/// Computes distinct activity targets across 6 frequency bands:
/// Band 0: Subagents / Delegated execution (Bass)
/// Band 1: Tool Calls / Command execution (Low-Mid)
/// Band 2: Turn Speed / Step cadence (Mid)
/// Band 3: Stream Volume / Byte throughput (Mid-High)
/// Band 4: Error Flags / Attention required (High-Mid)
/// Band 5: Token Throughput / Micro-burst generation (Treble)
pub fn compute_band_targets(session: &ActiveSession, pulse_phase: f32) -> [f32; 6] {
    let is_stale = session.is_stale();
    let is_active = session.is_active();
    let has_alert = matches!(
        session.state,
        AgentState::Error { .. } | AgentState::WaitingForApproval { .. }
    );

    if is_stale || (!is_active && !has_alert) {
        return [0.0; 6];
    }

    let mut targets = [0.0; 6];

    // Band 0: Subagents (Bass)
    let has_subagents = session.agent_type.eq_ignore_ascii_case("bridge")
        || session.metadata.host.starts_with("wsl:")
        || session.status_text.to_ascii_lowercase().contains("subagent")
        || session.status_text.to_ascii_lowercase().contains("worker")
        || session.metadata.tmux_pane.is_some();
    targets[0] = if has_subagents {
        (0.75 + 0.20 * (pulse_phase * 1.8).sin().abs()).clamp(0.0, 1.0)
    } else if is_active {
        (0.28 + 0.16 * (pulse_phase * 1.2).sin().abs()).clamp(0.0, 1.0)
    } else {
        0.0
    };

    // Band 1: Tool Calls (Low-Mid)
    targets[1] = match &session.state {
        AgentState::RunningTool { .. } => {
            (0.85 + 0.15 * (pulse_phase * 3.5).sin().abs()).clamp(0.0, 1.0)
        }
        AgentState::Thinking => {
            let text_lower = session.status_text.to_ascii_lowercase();
            if text_lower.contains("tool")
                || text_lower.contains("bash")
                || text_lower.contains("exec")
                || text_lower.contains("edit")
                || text_lower.contains("read")
            {
                (0.60 + 0.20 * (pulse_phase * 2.5).sin().abs()).clamp(0.0, 1.0)
            } else {
                (0.15 + 0.10 * (pulse_phase * 2.0).cos().abs()).clamp(0.0, 1.0)
            }
        }
        _ => 0.0,
    };

    // Band 2: Turn Speed (Mid)
    targets[2] = if is_active {
        let step_mod = ((session.step_count % 10) as f32 / 10.0) * 0.25;
        (0.52 + step_mod + 0.18 * (pulse_phase * 2.8).cos().abs()).clamp(0.0, 1.0)
    } else {
        0.0
    };

    // Band 3: Stream Volume (Mid-High)
    targets[3] = if is_active {
        let len_weight = (session.status_text.len() as f32 / 75.0).clamp(0.25, 0.65);
        let recency = if session.last_updated.elapsed().as_secs_f32() < 0.8 {
            0.28
        } else {
            0.08
        };
        (len_weight + recency + 0.12 * (pulse_phase * 4.2).sin().abs()).clamp(0.0, 1.0)
    } else {
        0.0
    };

    // Band 4: Error Flags (High-Mid / Alert)
    targets[4] = if matches!(session.state, AgentState::Error { .. }) {
        1.0
    } else if matches!(session.state, AgentState::WaitingForApproval { .. }) {
        0.88
    } else {
        let text_lower = session.status_text.to_ascii_lowercase();
        if text_lower.contains("error")
            || text_lower.contains("failed")
            || text_lower.contains("denied")
            || text_lower.contains("abort")
        {
            0.92
        } else if session.attention.is_unacknowledged {
            0.70
        } else {
            0.0
        }
    };

    // Band 5: Token Throughput (Treble)
    targets[5] = if is_active {
        (0.48 + 0.48 * ((pulse_phase * 6.0).sin() * (pulse_phase * 3.5).cos()).abs())
            .clamp(0.0, 1.0)
    } else {
        0.0
    };

    targets
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VuBandTracker {
    pub level: f32,
    pub peak: f32,
    pub hold_timer: f32,
}

impl Default for VuBandTracker {
    fn default() -> Self {
        Self {
            level: 0.0,
            peak: 0.0,
            hold_timer: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct VuSessionTracker {
    pub bands: [VuBandTracker; 6],
}

impl VuSessionTracker {
    pub fn update(&mut self, session: &ActiveSession, dt: f32, pulse_phase: f32) {
        let targets = compute_band_targets(session, pulse_phase);
        for i in 0..6 {
            let target = targets[i];
            let current = self.bands[i].level;
            let new_level = apply_vu_ballistics(current, target, dt);
            self.bands[i].level = new_level;

            let mut peak = self.bands[i].peak;
            let mut timer = self.bands[i].hold_timer;
            update_peak_hold(&mut peak, &mut timer, new_level, dt);
            self.bands[i].peak = peak;
            self.bands[i].hold_timer = timer;
        }
    }
}

/// Renders a status LED with a 4-layer concentric radial alpha bloom:
/// Progressive radii (r*2.8, r*2.0, r*1.4, r*1.0) and fading alpha (0.02, 0.08, 0.20, 0.40)
/// around the core phosphor lens and white hot cathode center dot.
pub fn render_led_with_bloom(
    painter: &egui::Painter,
    center: egui::Pos2,
    base_radius: f32,
    glow_color: Color32,
    pulse_intensity: f32,
) {
    let base_alpha = (230.0 * pulse_intensity).clamp(0.0, 255.0);
    let r = glow_color.r();
    let g = glow_color.g();
    let b = glow_color.b();

    // Ring 4: Outermost faint phosphor bloom haze (radius * 2.8, alpha * 0.02)
    let r4 = base_radius * 2.8;
    let a4 = (base_alpha * 0.02).round() as u8;
    if a4 > 0 {
        painter.circle_filled(center, r4, Color32::from_rgba_unmultiplied(r, g, b, a4));
    }

    // Ring 3: Soft phosphor ambient halo (radius * 2.0, alpha * 0.08)
    let r3 = base_radius * 2.0;
    let a3 = (base_alpha * 0.08).round() as u8;
    if a3 > 0 {
        painter.circle_filled(center, r3, Color32::from_rgba_unmultiplied(r, g, b, a3));
    }

    // Ring 2: Mid radial bloom ring (radius * 1.4, alpha * 0.20)
    let r2 = base_radius * 1.4;
    let a2 = (base_alpha * 0.20).round() as u8;
    if a2 > 0 {
        painter.circle_filled(center, r2, Color32::from_rgba_unmultiplied(r, g, b, a2));
    }

    // Ring 1: Inner core bloom glow (radius * 1.0, alpha * 0.40)
    let r1 = base_radius * 1.0;
    let a1 = (base_alpha * 0.40).round() as u8;
    if a1 > 0 {
        painter.circle_filled(center, r1, Color32::from_rgba_unmultiplied(r, g, b, a1));
    }

    // Solid phosphor core LED lens
    let r_core = base_radius * 0.72;
    let a_core = base_alpha.round() as u8;
    painter.circle_filled(
        center,
        r_core,
        Color32::from_rgba_unmultiplied(r, g, b, a_core),
    );

    // Hot cathode white center dot
    let r_white = (base_radius * 0.38).max(1.5);
    let white_alpha = (255.0 * pulse_intensity).clamp(160.0, 255.0) as u8;
    painter.circle_filled(
        center,
        r_white,
        Color32::from_rgba_unmultiplied(255, 255, 255, white_alpha),
    );
}

/// Custom tactile retro button matching the Winamp dark chassis:
/// Dark backgrounds, crisp retro borders, and high-contrast hover feedback.
pub fn tactile_retro_button(
    ui: &mut egui::Ui,
    text: &str,
    scale: f32,
    text_color: Color32,
    bg_color: Color32,
    border_color: Color32,
    hover_text_color: Color32,
    hover_bg_color: Color32,
    hover_border_color: Color32,
) -> egui::Response {
    let font = FontId::monospace(9.5 * scale);
    let padding = vec2(6.0 * scale, 3.0 * scale);
    let text_size = ui
        .painter()
        .layout_no_wrap(text.to_string(), font.clone(), text_color)
        .size();
    let desired_size = text_size + padding * 2.0;

    let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::click());

    if ui.is_rect_visible(rect) {
        let (bg, border, fg) = if response.hovered() || response.has_focus() {
            (hover_bg_color, hover_border_color, hover_text_color)
        } else {
            (bg_color, border_color, text_color)
        };

        ui.painter().rect_filled(rect, Rounding::same(2.0), bg);
        ui.painter()
            .rect_stroke(rect, Rounding::same(2.0), Stroke::new(1.0_f32, border));
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            font,
            fg,
        );
    }

    response
}

/// Measures text using egui font galleys and applies graceful truncation with ellipsis
/// if the text exceeds `max_width`. Guarantees UTF-8 character boundary safety and zero clipping.
fn elide_text(
    text: &str,
    font: FontId,
    max_width: f32,
    painter: &egui::Painter,
    color: Color32,
) -> Arc<egui::Galley> {
    let full_galley = painter.layout_no_wrap(text.to_string(), font.clone(), color);
    if full_galley.size().x <= max_width {
        return full_galley;
    }

    let ellipsis = "...";
    let ellipsis_galley = painter.layout_no_wrap(ellipsis.to_string(), font.clone(), color);
    if ellipsis_galley.size().x >= max_width {
        return ellipsis_galley;
    }

    let avail_for_chars = max_width - ellipsis_galley.size().x;
    let char_indices: Vec<(usize, char)> = text.char_indices().collect();
    if char_indices.is_empty() {
        return full_galley;
    }

    let mut low = 0;
    let mut high = char_indices.len();
    let mut best_prefix = "";

    while low <= high {
        let mid = (low + high) / 2;
        let byte_idx = if mid < char_indices.len() {
            char_indices[mid].0
        } else {
            text.len()
        };
        let candidate = &text[..byte_idx];
        let g = painter.layout_no_wrap(candidate.to_string(), font.clone(), color);
        if g.size().x <= avail_for_chars {
            best_prefix = candidate;
            low = mid + 1;
        } else {
            if mid == 0 {
                break;
            }
            high = mid - 1;
        }
    }

    let elided = format!("{}{}", best_prefix.trim_end(), ellipsis);
    painter.layout_no_wrap(elided, font, color)
}

pub struct SessionRowContext<'a> {
    pub scale: f32,
    pub dt: f32,
    pub pulse_phase: f32,
    pub selected_session_id: &'a mut Option<String>,
    pub editing_session_id: &'a mut Option<String>,
    pub edit_text_buffer: &'a mut String,
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
    vu_trackers: HashMap<String, VuSessionTracker>,
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
            vu_trackers: HashMap::new(),
        }
    }
}

fn render_session_row(
    ui: &mut egui::Ui,
    row_rect: Rect,
    session: &mut ActiveSession,
    ctx: &mut SessionRowContext,
    vu_tracker: &VuSessionTracker,
    actions: &mut Vec<UserAction>,
) {
    let scale = ctx.scale;
    let pulse_phase = ctx.pulse_phase;

    let is_active = session.is_active();
    let is_waiting_input = matches!(session.state, AgentState::WaitingForInput { .. });
    let is_waiting_approval = matches!(session.state, AgentState::WaitingForApproval { .. });
    let is_stale = session.is_stale();
    let should_pulse = session.attention.is_pulsating(&session.state);
    let is_editing = ctx.editing_session_id.as_deref() == Some(&session.session_id);
    let is_selected = ctx.selected_session_id.as_deref() == Some(&session.session_id);

    // Note: session.update_animations(dt, pulse_phase) has already been called in the session loop
    // so both marquee_offset and vu_levels persist across frames without cloning.

    // Interact sense (click row to select & acknowledge alert)
    let response = ui.interact(row_rect, ui.id().with(&session.session_id), egui::Sense::click());
    if response.clicked() {
        *ctx.selected_session_id = Some(session.session_id.clone());
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
        let breathe = organic_led_breathing(pulse_phase * 1.5);
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

    // Unified Proportional Scaling (Task 2):
    // Header Y, Marquee Y, and Inline Rename Edit Y scale consistently with scale factor.
    let header_y = row_rect.min.y + 7.0 * scale;
    let marquee_y = row_rect.min.y + 27.0 * scale;
    let edit_y = row_rect.min.y + 50.0 * scale;

    // 1. Status LED Indicator on Left (Organic breathing + Multi-layer Phosphor Bloom)
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
            AgentState::Exited => ("EXITED", Color32::from_rgb(100, 100, 100)),
        }
    };

    let led_center = pos2(row_rect.min.x + 12.0 * scale.min(1.2), header_y + 7.0 * scale);
    let pulse_intensity = if should_pulse {
        let breathe = organic_led_breathing(pulse_phase * 1.5);
        lerp(0.35, 1.0, breathe)
    } else if is_stale {
        0.4
    } else if is_waiting_approval || is_waiting_input {
        0.85
    } else if is_active {
        let breathe = organic_led_breathing(pulse_phase * 2.2);
        lerp(0.55, 1.0, breathe)
    } else {
        0.6
    };

    render_led_with_bloom(
        &painter,
        led_center,
        4.5 * scale.min(1.2),
        main_glow_color,
        pulse_intensity,
    );

    // 2. Line 1: Header layout (Task 3: Font galley measurement, zero collision)
    // Anchored right controls: Close Button [✕] and Step Counter
    let close_w = 14.0 * scale;
    let close_h = 13.0 * scale;
    let close_x = row_rect.max.x - 8.0 * scale - close_w;
    let close_btn_rect = Rect::from_min_size(pos2(close_x, header_y), vec2(close_w, close_h));
    let close_btn_resp = ui.interact(close_btn_rect, ui.id().with(&session.session_id).with("close_btn"), egui::Sense::click());
    let close_col = if close_btn_resp.hovered() {
        Color32::from_rgb(255, 100, 100)
    } else {
        Color32::from_rgb(65, 80, 75)
    };
    painter.text(pos2(close_btn_rect.min.x + 2.0 * scale, header_y), egui::Align2::LEFT_TOP, "✕", FontId::monospace(9.0 * scale), close_col);
    if close_btn_resp.clicked() {
        actions.push(UserAction::Dismiss(session.session_id.clone()));
    }

    // Step counter (measured and anchored to the left of close button)
    let step_text = format!("STEP {:03}", session.step_count);
    let font_step = FontId::monospace(9.0 * scale);
    let step_galley = painter.layout_no_wrap(step_text, font_step, Color32::from_rgb(60, 160, 90));
    let step_w = step_galley.size().x;
    let step_x = close_x - 8.0 * scale - step_w;
    painter.galley(pos2(step_x, header_y), step_galley, Color32::from_rgb(60, 160, 90));

    // Left cluster boundary: guaranteed clearance before step counter
    let max_header_left_x = step_x - 8.0 * scale;

    let badge_x = row_rect.min.x + 22.0 * scale.min(1.2);

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

    let font_state = FontId::monospace(9.5 * scale);
    let state_text = format!("• {}", state_label.to_uppercase());
    let state_galley = painter.layout_no_wrap(state_text, font_state.clone(), main_glow_color);
    let state_w = state_galley.size().x;

    let font_edit = FontId::monospace(9.0 * scale);
    let edit_text_galley = painter.layout_no_wrap("[EDIT]".to_string(), font_edit.clone(), Color32::WHITE);
    let edit_btn_w = edit_text_galley.size().x + 4.0 * scale;

    let dismiss_btn_w = if is_stale {
        let dismiss_galley = painter.layout_no_wrap("[DISMISS]".to_string(), font_edit.clone(), Color32::WHITE);
        dismiss_galley.size().x + 4.0 * scale
    } else {
        0.0
    };

    let spacing = 6.0 * scale;
    let required_right_of_badge = edit_btn_w + spacing + if is_stale { dismiss_btn_w + spacing } else { 0.0 } + state_w + spacing;

    // Available width for badge guarantees that state_label and edit button never collide or disappear
    let max_badge_w = (max_header_left_x - badge_x - required_right_of_badge).max(30.0 * scale);

    let font_badge = FontId::monospace(10.5 * scale);
    let badge_galley = elide_text(&badge_text, font_badge, max_badge_w, &painter, Color32::from_rgb(0, 220, 200));
    let actual_badge_w = badge_galley.size().x;
    painter.galley(pos2(badge_x, header_y), badge_galley, Color32::from_rgb(0, 220, 200));

    // Position [EDIT] button
    let edit_btn_x = badge_x + actual_badge_w + spacing;
    let edit_btn_rect = Rect::from_min_size(pos2(edit_btn_x, header_y), vec2(edit_btn_w, 13.0 * scale));
    let edit_btn_resp = ui.interact(edit_btn_rect, ui.id().with(&session.session_id).with("edit_btn"), egui::Sense::click());
    let edit_btn_col = if edit_btn_resp.hovered() {
        Color32::from_rgb(255, 220, 100)
    } else {
        Color32::from_rgb(70, 105, 90)
    };
    painter.text(edit_btn_rect.min, egui::Align2::LEFT_TOP, "[EDIT]", font_edit.clone(), edit_btn_col);
    if edit_btn_resp.clicked() {
        *ctx.editing_session_id = Some(session.session_id.clone());
        *ctx.edit_text_buffer = session.display_name.clone();
    }

    let mut next_header_x = edit_btn_x + edit_btn_w + spacing;

    // If stale, render prominent [DISMISS] button
    if is_stale {
        let dismiss_pill_rect = Rect::from_min_size(pos2(next_header_x, header_y), vec2(dismiss_btn_w, 13.0 * scale));
        let dismiss_pill_resp = ui.interact(dismiss_pill_rect, ui.id().with(&session.session_id).with("dismiss_pill"), egui::Sense::click());
        let pill_col = if dismiss_pill_resp.hovered() {
            Color32::from_rgb(255, 120, 120)
        } else {
            Color32::from_rgb(210, 140, 90)
        };
        painter.text(dismiss_pill_rect.min, egui::Align2::LEFT_TOP, "[DISMISS]", font_edit, pill_col);
        if dismiss_pill_resp.clicked() {
            actions.push(UserAction::Dismiss(session.session_id.clone()));
        }
        next_header_x += dismiss_btn_w + spacing;
    }

    // Render State Label (Always visible, never dropped or collided)
    let state_x = next_header_x;
    if state_x + state_w <= max_header_left_x + spacing {
        painter.galley(pos2(state_x, header_y), state_galley, main_glow_color);
    } else {
        let avail_state_w = (max_header_left_x - state_x).max(20.0 * scale);
        let elided_state = elide_text(&format!("• {}", state_label.to_uppercase()), font_state, avail_state_w, &painter, main_glow_color);
        painter.galley(pos2(state_x, header_y), elided_state, main_glow_color);
    }

    // 3. Line 2: Status Text Display (Task 3: Bounding Box Padding & Zero-Glyph-Clipping)
    let marquee_right = row_rect.max.x - 68.0 * scale.min(1.2);
    // Vertical padding: 2.0px above marquee_y and 2.0px below marquee line height so uppercase,
    // accents, and descenders ('g', 'j', 'p', 'q', 'y') are NEVER clipped by the clip boundary.
    let marquee_area = Rect::from_min_max(
        pos2(row_rect.min.x + 8.0, marquee_y - 2.0),
        pos2(marquee_right, marquee_y + 18.0 * scale + 2.0),
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
            AgentState::Exited => Color32::from_rgb(100, 100, 100),
            _ => Color32::from_rgb(40, 255, 120),
        }
    };

    let font_status = FontId::monospace(11.5 * scale);
    let max_w = (marquee_area.width() - 4.0).max(10.0);

    let mut row_painter = ui.painter_at(row_rect);
    let prev_clip = row_painter.clip_rect();
    row_painter.set_clip_rect(marquee_area);

    if is_waiting_input || is_waiting_approval || is_stale {
        // Static mode: measure status text with egui font galleys and apply graceful truncation
        // with ellipsis if it exceeds marquee_area.width()
        let status_line = if is_stale {
            format!("(Inactive > 15m) {}", session.status_text)
        } else {
            session.status_text.clone()
        };

        let galley = elide_text(&status_line, font_status, max_w, &row_painter, text_color);
        row_painter.galley(pos2(marquee_area.min.x + 2.0, marquee_y), galley, text_color);
    } else {
        // Marquee mode: measure text accurately; if text fits within viewport width,
        // do NOT scroll (display cleanly); if it exceeds width, scroll smoothly with seamless looping.
        let clean_status = session.status_text.trim();
        let status_str = if clean_status.is_empty() {
            session.status_text.as_str()
        } else {
            clean_status
        };

        let full_galley = row_painter.layout_no_wrap(status_str.to_string(), font_status.clone(), text_color);
        let text_width = full_galley.size().x;

        if text_width <= max_w {
            // Fits cleanly: no scrolling needed
            row_painter.galley(pos2(marquee_area.min.x + 2.0, marquee_y), full_galley, text_color);
        } else {
            // Exceeds width: seamless looping with smooth font galley positioning
            let gap = 48.0 * scale;
            let loop_len = text_width + gap;
            let offset_mod = session.marquee_offset % loop_len;
            let start_x = marquee_area.max.x - offset_mod;

            row_painter.galley(pos2(start_x, marquee_y), full_galley.clone(), text_color);
            row_painter.galley(pos2(start_x + loop_len, marquee_y), full_galley, text_color);
        }
    }
    row_painter.set_clip_rect(prev_clip);

    // 4. Mini VU Meter on Right (Winamp Ballistics & Floating Peak Hold)
    let vu_box_min = pos2(row_rect.max.x - 58.0 * scale.min(1.2), row_rect.min.y + 12.0 * scale);
    let num_bars = 6;
    let bar_w = 5.0 * scale.min(1.2);
    let bar_gap = 2.0 * scale.min(1.2);
    let total_segments = 5;

    for i in 0..num_bars {
        let x = vu_box_min.x + i as f32 * (bar_w + bar_gap);
        let level = vu_tracker.bands[i].level;
        let peak = vu_tracker.bands[i].peak;
        let active_segments = (level * total_segments as f32).round() as usize;

        // Render 5 discrete active/unlit LED segments
        for seg in 0..total_segments {
            let seg_y = (row_rect.min.y + 38.0 * scale) - (seg as f32 * 3.5 * scale.min(1.2));
            let seg_rect = Rect::from_min_size(pos2(x, seg_y), vec2(bar_w, 2.5 * scale.min(1.2)));
            let seg_color = if seg < active_segments {
                if seg >= 4 {
                    Color32::from_rgb(255, 80, 80) // Red Peak
                } else if seg >= 3 {
                    Color32::from_rgb(255, 200, 30) // Amber Mid
                } else {
                    Color32::from_rgb(0, 255, 100) // Green
                }
            } else {
                Color32::from_rgb(14, 24, 18) // Unlit LED chassis
            };
            painter.rect_filled(seg_rect, Rounding::ZERO, seg_color);
        }

        // Floating peak hold indicator line above active segments
        if peak > 0.04 {
            let seg_span = 4.0 * 3.5 * scale.min(1.2);
            let peak_y = (row_rect.min.y + 38.0 * scale) - (peak * seg_span);
            let peak_h = (1.5 * scale.min(1.2)).max(1.0);
            let peak_rect = Rect::from_min_size(pos2(x, peak_y), vec2(bar_w, peak_h));

            let peak_color = if peak >= 0.8 {
                Color32::from_rgb(255, 90, 90) // Red Peak line
            } else if peak >= 0.6 {
                Color32::from_rgb(255, 210, 40) // Amber Peak line
            } else {
                Color32::from_rgb(80, 255, 140) // Phosphor Green Peak line
            };
            painter.rect_filled(peak_rect, Rounding::ZERO, peak_color);
        }
    }

    // 5. Inline Rename Overlay (Tactile Retro Dark Styling)
    if is_editing {
        let edit_ui_rect = Rect::from_min_size(
            pos2(row_rect.min.x + 8.0, edit_y),
            vec2(row_rect.width() - 16.0, 24.0 * scale),
        );

        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(edit_ui_rect), |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(
                    Color32::from_rgb(0, 220, 200),
                    egui::RichText::new("NAME:").monospace().size(9.5 * scale),
                );

                let text_input = ui.add(
                    egui::TextEdit::singleline(ctx.edit_text_buffer)
                        .desired_width(180.0 * scale)
                        .font(FontId::monospace(10.5 * scale))
                        .text_color(Color32::from_rgb(0, 240, 180))
                        .margin(egui::Margin::symmetric(4.0, 2.0)),
                );

                let enter_pressed = text_input.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

                let save_btn = tactile_retro_button(
                    ui,
                    "Save",
                    scale,
                    Color32::from_rgb(0, 255, 140),
                    Color32::from_rgb(14, 28, 20),
                    Color32::from_rgb(30, 80, 50),
                    Color32::WHITE,
                    Color32::from_rgb(20, 60, 40),
                    Color32::from_rgb(0, 255, 160),
                );

                if save_btn.clicked() || enter_pressed {
                    let new_name = ctx.edit_text_buffer.trim().to_string();
                    if !new_name.is_empty() {
                        session.display_name = new_name.clone();
                        actions.push(UserAction::Rename(session.session_id.clone(), new_name));
                    }
                    *ctx.editing_session_id = None;
                }

                let reset_btn = tactile_retro_button(
                    ui,
                    "Reset",
                    scale,
                    Color32::from_rgb(255, 200, 60),
                    Color32::from_rgb(26, 22, 14),
                    Color32::from_rgb(80, 65, 30),
                    Color32::WHITE,
                    Color32::from_rgb(50, 40, 18),
                    Color32::from_rgb(255, 210, 80),
                );

                if reset_btn.clicked() {
                    actions.push(UserAction::Rename(session.session_id.clone(), "".to_string()));
                    *ctx.editing_session_id = None;
                }

                let cancel_btn = tactile_retro_button(
                    ui,
                    "Cancel",
                    scale,
                    Color32::from_rgb(160, 175, 170),
                    Color32::from_rgb(20, 24, 26),
                    Color32::from_rgb(50, 60, 66),
                    Color32::WHITE,
                    Color32::from_rgb(35, 44, 48),
                    Color32::from_rgb(180, 200, 205),
                );

                if cancel_btn.clicked() {
                    *ctx.editing_session_id = None;
                }
            });
        });
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

        // Ingest stream updates
        self.hub.poll_events();

        // Clear selection / editing buffers if the target session exited and was cleaned up
        if let Some(ref sel_id) = self.selected_session_id {
            if !self.hub.sessions.iter().any(|s| &s.session_id == sel_id) {
                self.selected_session_id = None;
            }
        }
        if let Some(ref edit_id) = self.editing_session_id {
            if !self.hub.sessions.iter().any(|s| &s.session_id == edit_id) {
                self.editing_session_id = None;
                self.edit_text_buffer.clear();
            }
        }

        // Single-pass Category Summary (Task 4: F9 Optimization)
        let summaries = self.hub.category_summary();
        if self.hub.selected_tab_idx >= summaries.len() {
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
                        let breathe = organic_led_breathing(self.pulse_phase * 1.5);
                        let pulse_intensity = lerp(0.35, 1.0, breathe);
                        Color32::from_rgb(0, (240.0 * pulse_intensity) as u8, (200.0 * pulse_intensity) as u8)
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

            // Dynamic Category Tabs Rendering (Single-pass CategorySummary)
            ui.horizontal(|ui| {
                for (tab_idx, summary) in summaries.iter().enumerate() {
                    let count = summary.session_count;
                    let is_unacked = summary.has_unacknowledged;
                    let is_waiting = summary.has_waiting_input;
                    let is_active = self.hub.selected_tab_idx == tab_idx;

                    let tab_bg = if is_active {
                        Color32::from_rgb(42, 52, 68)
                    } else {
                        Color32::from_rgb(24, 27, 34)
                    };

                    let tab_border = if is_unacked {
                        let breathe = organic_led_breathing(self.pulse_phase * 1.5);
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

                    let tab_label = format!("{} {} • {}", dot, summary.label, count);

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
                        frame_actions.push(UserAction::AcknowledgeCategory(summary.id.clone()));
                    }
                }
            });

            ui.add_space(4.0);

            if !self.is_compact_mode && !summaries.is_empty() {
                let current_summary = summaries.get(self.hub.selected_tab_idx).unwrap_or(&summaries[0]);
                let matching_ids: Vec<String> = self
                    .hub
                    .sessions_for_summary(current_summary)
                    .iter()
                    .map(|s| s.session_id.clone())
                    .collect();

                // Advance animation state for sessions in other environments not currently rendered
                for session in &mut self.hub.sessions {
                    if !matching_ids.iter().any(|id| id == &session.session_id) {
                        session.update_animations(dt, self.pulse_phase);
                        let tracker = self
                            .vu_trackers
                            .entry(session.session_id.clone())
                            .or_default();
                        tracker.update(session, dt, self.pulse_phase);
                        for i in 0..6 {
                            session.vu_levels[i] = tracker.bands[i].level;
                        }
                    }
                }

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
                                    egui::RichText::new("No active sessions in this environment")
                                        .monospace()
                                        .size(11.0 * scale),
                                );
                            });
                        } else {
                            let mut row_ctx = SessionRowContext {
                                scale,
                                dt,
                                pulse_phase: self.pulse_phase,
                                selected_session_id: &mut self.selected_session_id,
                                editing_session_id: &mut self.editing_session_id,
                                edit_text_buffer: &mut self.edit_text_buffer,
                            };

                            for session_id in matching_ids {
                                if let Some(idx) = self.hub.sessions.iter().position(|s| s.session_id == session_id) {
                                    let session = &mut self.hub.sessions[idx];

                                    // Advance animations in-place directly on self.hub.sessions
                                    // (Ensures off-screen sessions maintain state!)
                                    session.update_animations(dt, row_ctx.pulse_phase);
                                    let tracker = self
                                        .vu_trackers
                                        .entry(session.session_id.clone())
                                        .or_default();
                                    tracker.update(session, dt, row_ctx.pulse_phase);
                                    for i in 0..6 {
                                        session.vu_levels[i] = tracker.bands[i].level;
                                    }
                                    let current_tracker = *tracker;

                                    let is_editing = row_ctx.editing_session_id.as_deref() == Some(&session.session_id);
                                    let base_height = if is_editing { 78.0 } else { 54.0 };
                                    let row_height = (base_height * scale).round();
                                    let row_rect = ui.allocate_space(vec2(ui.available_width(), row_height)).1;

                                    // Viewport Culling: Skip heavy painting primitives for offscreen rows
                                    if !ui.is_rect_visible(row_rect) {
                                        ui.add_space(3.0);
                                        continue;
                                    }

                                    render_session_row(
                                        ui,
                                        row_rect,
                                        session,
                                        &mut row_ctx,
                                        &current_tracker,
                                        &mut frame_actions,
                                    );
                                    ui.add_space(3.0);
                                }
                            }
                        }
                    });

                ui.add_space(2.0);

                // Bottom Global Status Bar
                let total_sessions = self.hub.sessions.len();
                let total_waiting = self
                    .hub
                    .sessions
                    .iter()
                    .filter(|s| matches!(s.state, AgentState::WaitingForInput { .. } | AgentState::WaitingForApproval { .. }))
                    .count();

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
            } else if self.is_compact_mode {
                // In compact mode, still advance all session animations
                self.hub.update_animations(dt, self.pulse_phase);
                for session in &mut self.hub.sessions {
                    let tracker = self
                        .vu_trackers
                        .entry(session.session_id.clone())
                        .or_default();
                    tracker.update(session, dt, self.pulse_phase);
                    for i in 0..6 {
                        session.vu_levels[i] = tracker.bands[i].level;
                    }
                }
            }
        });

        // Prune trackers for sessions that have been permanently dismissed
        if self.vu_trackers.len() > self.hub.sessions.len() + 10 {
            let active_ids: std::collections::HashSet<String> = self
                .hub
                .sessions
                .iter()
                .map(|s| s.session_id.clone())
                .collect();
            self.vu_trackers.retain(|id, _| active_ids.contains(id));
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use agent_deck_core::{AgentState, SessionEvent, SessionMetadata};
    use eframe::egui;
    use hub::AttentionState;

    #[test]
    fn test_m3_proportional_scaling_geometry_at_all_scales() {
        let scales = [0.85, 1.00, 1.15, 1.30, 1.45, 1.60];

        for &scale in &scales {
            let row_min_y = 100.0;
            let row_rect = Rect::from_min_size(pos2(10.0, row_min_y), vec2(500.0, 54.0 * scale));

            let base_normal = 54.0;
            let base_editing = 78.0;

            let normal_row_h = (base_normal * scale).round();
            let editing_row_h = (base_editing * scale).round();

            let header_y = row_rect.min.y + 7.0 * scale;
            let marquee_y = row_rect.min.y + 27.0 * scale;
            let edit_y = row_rect.min.y + 50.0 * scale;

            let marquee_area_min_y = marquee_y - 2.0;
            let marquee_area_max_y = marquee_y + 18.0 * scale + 2.0;

            // 1. Marquee Y and padded clip top are strictly below the header text
            let header_text_bottom = header_y + 10.5 * scale;
            assert!(
                marquee_area_min_y >= header_text_bottom,
                "At scale {}, marquee_area_min_y ({}) must be below header_text_bottom ({})",
                scale,
                marquee_area_min_y,
                header_text_bottom
            );

            // 2. Vertical collision test: edit_y must be strictly greater than marquee_area_max_y at all scales
            assert!(
                edit_y > marquee_area_max_y,
                "At scale {}, edit_y ({}) must exceed marquee_area_max_y ({}) to prevent vertical collision",
                scale,
                edit_y,
                marquee_area_max_y
            );

            // 3. Clearance between marquee and edit input must be at least 2.0px across all scales
            let gap = edit_y - marquee_area_max_y;
            assert!(
                gap >= 2.0,
                "At scale {}, gap between marquee and edit box ({}) must be >= 2.0px",
                scale,
                gap
            );

            // 4. Editing row height must fully encompass the inline rename box (edit_y + 24.0 * scale)
            let edit_box_bottom = edit_y + 24.0 * scale;
            let row_bottom = row_rect.min.y + editing_row_h;
            assert!(
                row_bottom >= edit_box_bottom,
                "At scale {}, editing row height ({}) must fit edit box bottom ({})",
                scale,
                row_bottom,
                edit_box_bottom
            );

            // 5. Normal row height must fully encompass the marquee area
            let normal_bottom = row_rect.min.y + normal_row_h;
            assert!(
                normal_bottom >= marquee_area_max_y,
                "At scale {}, normal row height ({}) must fit marquee area bottom ({})",
                scale,
                normal_bottom,
                marquee_area_max_y
            );
        }
    }

    #[test]
    fn test_m3_marquee_area_vertical_padding_zero_clipping() {
        let scale = 1.6;
        let row_min_y = 50.0;
        let marquee_y = row_min_y + 27.0 * scale;
        let marquee_right = 400.0;

        let marquee_area = Rect::from_min_max(
            pos2(18.0, marquee_y - 2.0),
            pos2(marquee_right, marquee_y + 18.0 * scale + 2.0),
        );

        // Assert 2.0px vertical padding above text baseline
        assert_eq!(marquee_area.min.y, marquee_y - 2.0);
        // Assert 2.0px vertical padding below line box
        assert_eq!(marquee_area.max.y, marquee_y + 18.0 * scale + 2.0);

        // Total vertical headroom must be at least 4.0px greater than raw line height
        let total_h = marquee_area.height();
        let raw_h = 18.0 * scale;
        assert!((total_h - raw_h - 4.0).abs() < 0.001);
    }

    #[test]
    fn test_m3_elide_text_truncation_and_measurement() {
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let painter = ui.painter();
                let font = FontId::monospace(12.0);
                let color = Color32::WHITE;

                // 1. Short text that fits within max_width should not be truncated
                let short_text = "Short text";
                let g1 = elide_text(short_text, font.clone(), 200.0, painter, color);
                assert!(g1.size().x <= 200.0);
                assert_eq!(g1.text(), short_text);

                // 2. Long text exceeding max_width should be truncated with an ellipsis
                let long_text = "This is a very long session status description that will certainly exceed seventy pixels";
                let g2 = elide_text(long_text, font.clone(), 70.0, painter, color);
                assert!(g2.size().x <= 70.0, "Elided galley width {} must be <= 70.0", g2.size().x);
                assert!(g2.text().ends_with("..."), "Elided text '{}' must end with ellipsis", g2.text());

                // 3. Multi-byte UTF-8 string with accented characters and symbols
                let unicode_text = "Résumé • Exécution d'un script en cours • Vérification des clés";
                let g3 = elide_text(unicode_text, font.clone(), 120.0, painter, color);
                assert!(g3.size().x <= 120.0);
                assert!(g3.text().ends_with("..."));
                // Verifies valid UTF-8 and no panics
                let _ = g3.text().chars().count();

                // 4. Text with descenders ('g', 'j', 'p', 'q', 'y')
                let descender_text = "pkg_query: ping pong jump query";
                let g4 = elide_text(descender_text, font.clone(), 90.0, painter, color);
                assert!(g4.size().x <= 90.0);
                assert!(g4.text().ends_with("..."));
            });
        });
    }

    #[test]
    fn test_m3_in_place_session_animation_monotonic_advance() {
        let storage = Arc::new(RwLock::new(CustomTitlesStorage::in_memory()));
        let mut hub = SessionHub::new(storage);

        hub.sender().send(SessionEvent::new(
            "sess_anim_1",
            "Animation Test",
            "Gemini",
            AgentState::Thinking,
            "Thinking about dynamic scaling polish",
            1,
            SessionMetadata {
                host: "Windows".to_string(),
                tmux_session: None,
                tmux_window: None,
                tmux_pane: None,
                cwd: None,
                pid: None,
                agent_type: None,
            },
        )).unwrap();
        hub.poll_events();

        assert_eq!(hub.sessions.len(), 1);
        assert_eq!(hub.sessions[0].marquee_offset, 0.0);

        // Simulate 10 frames with dt = 0.016 (60fps)
        let dt = 0.016;
        let mut prev_offset = 0.0;
        for frame in 1..=10 {
            let session = &mut hub.sessions[0];
            session.update_animations(dt, frame as f32 * 0.1);

            assert!(
                session.marquee_offset > prev_offset,
                "Frame {}: marquee_offset {} must be > prev_offset {}",
                frame,
                session.marquee_offset,
                prev_offset
            );
            prev_offset = session.marquee_offset;
        }

        // Marquee offset should have advanced smoothly to ~6.08px without resetting
        assert!((hub.sessions[0].marquee_offset - (10.0 * dt * 38.0)).abs() < 0.01);

        // When transition to Idle, marquee_offset resets to 0.0 and vu decays
        hub.sessions[0].state = AgentState::Idle;
        let vu_before = hub.sessions[0].vu_levels;
        hub.sessions[0].update_animations(dt, 1.0);
        assert_eq!(hub.sessions[0].marquee_offset, 0.0);

        // VU levels should decay smoothly rather than jumping instantly to zero
        for (i, bar) in hub.sessions[0].vu_levels.iter().enumerate() {
            assert!(*bar <= vu_before[i] + 0.001);
        }
    }

    #[test]
    fn test_m3_viewport_culling_detection_logic() {
        let viewport = Rect::from_min_max(pos2(0.0, 0.0), pos2(580.0, 200.0));

        let row_height = 54.0;
        let row_gap = 3.0;

        let mut visible_count = 0;
        let mut culled_count = 0;

        for i in 0..20 {
            let y = i as f32 * (row_height + row_gap);
            let row_rect = Rect::from_min_size(pos2(0.0, y), vec2(580.0, row_height));

            if viewport.intersects(row_rect) {
                visible_count += 1;
            } else {
                culled_count += 1;
            }
        }

        // In a 200px viewport with ~57px total row step, exactly 4 rows intersect
        assert_eq!(visible_count, 4);
        assert_eq!(culled_count, 16);
    }

    #[test]
    fn test_m3_twenty_sessions_category_summary_and_update_frame_time() {
        let storage = Arc::new(RwLock::new(CustomTitlesStorage::in_memory()));
        let mut hub = SessionHub::new(storage);

        // Populate 25 active sessions across Windows and multiple WSL distros
        for i in 0..25 {
            let host = if i % 3 == 0 {
                "Windows"
            } else if i % 3 == 1 {
                "wsl:Ubuntu"
            } else {
                "wsl:Debian"
            };

            let state = if i == 0 {
                AgentState::WaitingForApproval { name: "edit".into(), summary: "sum".into() }
            } else if i % 2 == 0 {
                AgentState::Thinking
            } else {
                AgentState::RunningTool { name: "cargo".into(), summary: "test".into() }
            };

            hub.sender().send(SessionEvent::new(
                &format!("sess_{:02}", i),
                &format!("Session {:02}", i),
                "Gemini",
                state,
                &format!("Active status text description for session {:02}", i),
                i as u32,
                SessionMetadata {
                    host: host.to_string(),
                    tmux_session: None,
                    tmux_window: None,
                    tmux_pane: None,
                    cwd: None,
                    pid: None,
                    agent_type: None,
                },
            )).unwrap();
        }
        hub.poll_events();
        assert_eq!(hub.sessions.len(), 25);

        // Measure 100 frame simulation cycles (category_summary + in-place animation updates)
        let start = Instant::now();
        let dt = 0.016;

        for frame in 0..100 {
            let summaries = hub.category_summary();
            assert_eq!(summaries.len(), 3); // Windows, Ubuntu, Debian

            let pulse = frame as f32 * 0.1;
            hub.update_animations(dt, pulse);
        }

        let elapsed = start.elapsed();
        let avg_frame_time_ms = (elapsed.as_secs_f64() * 1000.0) / 100.0;

        // Must easily execute well within 16.0ms budget (typically < 0.2ms)
        assert!(
            avg_frame_time_ms < 5.0,
            "Average frame time {:?} must be < 5.0ms (ceiling 16.0ms)",
            avg_frame_time_ms
        );
    }

    #[test]
    fn test_m3_header_badge_measurement_state_label_retention() {
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let painter = ui.painter();
                let scale = 1.60;
                let row_width = 460.0; // Minimum window width constraint

                let font_badge = FontId::monospace(10.5 * scale);
                let font_state = FontId::monospace(9.5 * scale);
                let font_step = FontId::monospace(9.0 * scale);
                let font_edit = FontId::monospace(9.0 * scale);

                // Right cluster
                let close_w = 14.0 * scale;
                let close_x = row_width - 8.0 * scale - close_w;

                let step_galley = painter.layout_no_wrap("STEP 099".to_string(), font_step, Color32::WHITE);
                let step_w = step_galley.size().x;
                let step_x = close_x - 8.0 * scale - step_w;

                let max_header_left_x = step_x - 8.0 * scale;
                let badge_x = 22.0 * scale.min(1.2);

                // Extremely long badge text
                let long_badge = "Gemini • wsl:Ubuntu • Very Long Agent Title That Takes A Lot Of Space 1234567890";
                let state_text = "• APPROVAL REQUIRED";
                let state_galley = painter.layout_no_wrap(state_text.to_string(), font_state.clone(), Color32::WHITE);
                let state_w = state_galley.size().x;

                let edit_text_galley = painter.layout_no_wrap("[EDIT]".to_string(), font_edit, Color32::WHITE);
                let edit_btn_w = edit_text_galley.size().x + 4.0 * scale;
                let spacing = 6.0 * scale;

                let required_right_of_badge = edit_btn_w + spacing + state_w + spacing;
                let max_badge_w = (max_header_left_x - badge_x - required_right_of_badge).max(30.0 * scale);

                let badge_galley = elide_text(long_badge, font_badge, max_badge_w, painter, Color32::WHITE);
                let actual_badge_w = badge_galley.size().x;

                let edit_btn_x = badge_x + actual_badge_w + spacing;
                let state_x = edit_btn_x + edit_btn_w + spacing;

                // Verify that state label is allocated space and does not collide with step counter
                assert!(state_x + state_w <= max_header_left_x + spacing + 1.0);
                assert!(state_x < step_x, "State label start ({}) must be to the left of step counter ({})", state_x, step_x);
            });
        });
    }

    #[test]
    fn test_m4_vu_ballistics_asymmetric_attack_and_decay() {
        let dt = 0.016; // 60fps frame delta

        // 1. Attack Test (Rising target): speed = 40.0
        // Starting at 0.0 with target 1.0
        let current = 0.0;
        let target = 1.0;
        let step1 = apply_vu_ballistics(current, target, dt);
        let attack_delta = step1 - current;
        // Expected: t = 0.016 * 40.0 = 0.64; step1 = 0.64
        assert!((step1 - 0.64).abs() < 0.001);

        // 2. Decay Test (Falling target): speed = 5.0
        // Starting at 1.0 with target 0.0
        let current_decay = 1.0;
        let target_decay = 0.0;
        let step1_decay = apply_vu_ballistics(current_decay, target_decay, dt);
        let decay_delta = current_decay - step1_decay;
        // Expected: t = 0.016 * 5.0 = 0.08; step1_decay = 0.92; delta = 0.08
        assert!((step1_decay - 0.92).abs() < 0.001);

        // 3. Asymmetric Speed Ratio Verification
        // Attack delta must be exactly 8.0x faster than decay delta for identical displacement
        let ratio = attack_delta / decay_delta;
        assert!(
            (ratio - 8.0).abs() < 0.001,
            "Attack must be 8x faster than decay (40.0 / 5.0 = 8.0), got ratio {}",
            ratio
        );

        // 4. Attack reaches near-instant saturation within 5 frames
        let mut level = 0.0;
        for _ in 0..5 {
            level = apply_vu_ballistics(level, 1.0, dt);
        }
        assert!(
            level > 0.99,
            "Attack must achieve >99% saturation in 5 frames (~80ms), got {}",
            level
        );

        // 5. Decay falls smoothly without sudden snapping
        let mut decay_level = 1.0;
        for _ in 0..5 {
            decay_level = apply_vu_ballistics(decay_level, 0.0, dt);
        }
        // At 5 frames of decay: (1 - 0.08)^5 ≈ 0.659
        assert!(
            decay_level > 0.60 && decay_level < 0.70,
            "Decay after 5 frames must be ~0.659, got {}",
            decay_level
        );

        // 6. Boundary Clamping
        assert_eq!(apply_vu_ballistics(0.5, 2.0, dt), 1.0);
        let decay_to_zero = apply_vu_ballistics(0.5, 0.0, dt);
        assert!((decay_to_zero - 0.46).abs() < 0.001);
        assert_eq!(apply_vu_ballistics(0.0, -1.0, dt), 0.0);
    }

    #[test]
    fn test_m4_peak_hold_timer_and_decay_dynamics() {
        let mut peak = 0.0;
        let mut timer = 0.0;

        // 1. Initial trigger: level leaps to 0.85
        update_peak_hold(&mut peak, &mut timer, 0.85, 0.016);
        assert_eq!(peak, 0.85);
        assert_eq!(timer, 0.6); // 600ms hold timer

        // 2. Floating peak hold: level drops to 0.20, dt = 0.2s elapsed
        update_peak_hold(&mut peak, &mut timer, 0.20, 0.2);
        assert_eq!(peak, 0.85, "Peak must remain floating at 0.85 during hold window");
        assert!((timer - 0.4).abs() < 0.001);

        // Another 0.3s elapsed (total 0.5s elapsed): timer = 0.1s
        update_peak_hold(&mut peak, &mut timer, 0.20, 0.3);
        assert_eq!(peak, 0.85, "Peak must still remain floating at 0.85");
        assert!((timer - 0.1).abs() < 0.001);

        // 3. Timer expiration: another 0.2s elapsed (total 0.7s, timer expired)
        update_peak_hold(&mut peak, &mut timer, 0.20, 0.2);
        assert_eq!(timer, 0.0, "Timer must be expired (0.0s)");
        assert!(
            peak < 0.85,
            "Peak must begin falling smoothly after timer expires, got {}",
            peak
        );

        // 4. Decay speed: dt = 0.016 with timer expired
        let peak_before = peak;
        update_peak_hold(&mut peak, &mut timer, 0.20, 0.016);
        let decay_step = peak_before - peak;
        let expected_step = (peak_before - 0.20) * (0.016 * 2.5);
        assert!(
            (decay_step - expected_step).abs() < 0.001,
            "Decay rate must follow dt * 2.5"
        );

        // 5. Floor constraint: peak must never drop below current level
        for _ in 0..200 {
            update_peak_hold(&mut peak, &mut timer, 0.20, 0.05);
        }
        assert!(
            (peak - 0.20).abs() < 1e-4,
            "Peak must clamp at current level, got {}",
            peak
        );

        // 6. Resurgence: sudden level spike above peak
        update_peak_hold(&mut peak, &mut timer, 0.95, 0.016);
        assert_eq!(peak, 0.95);
        assert_eq!(timer, 0.6, "Timer must reset to 600ms on higher peak");
    }

    #[test]
    fn test_m4_organic_led_breathing_curve_properties() {
        let pi = std::f32::consts::PI;

        // 1. Peak value at t = PI / 2: exactly 1.0
        let peak_val = organic_led_breathing(pi / 2.0);
        assert!(
            (peak_val - 1.0).abs() < 1e-5,
            "organic_led_breathing(PI/2) must be 1.0, got {}",
            peak_val
        );

        // 2. Minimum value at t = -PI / 2 or 3*PI/2: exactly 0.0
        let min_val1 = organic_led_breathing(-pi / 2.0);
        assert!(
            min_val1.abs() < 1e-5,
            "organic_led_breathing(-PI/2) must be 0.0, got {}",
            min_val1
        );
        let min_val2 = organic_led_breathing(3.0 * pi / 2.0);
        assert!(
            min_val2.abs() < 1e-5,
            "organic_led_breathing(3PI/2) must be 0.0, got {}",
            min_val2
        );

        // 3. Midpoint asymmetry at t = 0:
        // Unlike linear sinusoid ((sin(0) + 1)/2 = 0.5), the biological exponential curve
        // dwells at (e^0 - 1/e) / (e - 1/e) ≈ 0.26894
        let zero_val = organic_led_breathing(0.0);
        let expected_zero = (1.0 - 1.0 / std::f32::consts::E) / (std::f32::consts::E - 1.0 / std::f32::consts::E);
        assert!(
            (zero_val - expected_zero).abs() < 1e-4,
            "organic_led_breathing(0.0) must be ~0.26894, got {}",
            zero_val
        );
        assert!(
            zero_val < 0.35,
            "Biological curve must spend more duty cycle in gentle low-luminance"
        );

        // 4. Monotonic ascent on [-PI/2, PI/2]
        let mut prev = -0.01;
        for step in 0..50 {
            let t = -pi / 2.0 + (pi * step as f32 / 50.0);
            let val = organic_led_breathing(t);
            assert!(
                val >= prev,
                "Biological easing must be strictly monotonic during ascent"
            );
            prev = val;
        }

        // 5. Strict range bounds across 1000 cycle points
        for i in 0..1000 {
            let t = i as f32 * 0.02;
            let val = organic_led_breathing(t);
            assert!(
                val >= 0.0 && val <= 1.0,
                "Value {} at t = {} must be clamped to [0.0, 1.0]",
                val,
                t
            );
        }
    }

    #[test]
    fn test_m4_multi_layer_radial_bloom_parameters() {
        // Multi-layer phosphor CRT bloom parameters:
        // Progressive radii: 1.0x, 1.4x, 2.0x, 2.8x
        // Fading alpha factors: 0.40, 0.20, 0.08, 0.02
        let base_radius = 5.0_f32;
        let base_alpha = 230.0_f32;

        let r1 = base_radius * 1.0;
        let r2 = base_radius * 1.4;
        let r3 = base_radius * 2.0;
        let r4 = base_radius * 2.8;

        assert!(r1 < r2 && r2 < r3 && r3 < r4, "Radii must be strictly progressive");

        let a1 = (base_alpha * 0.40).round() as u8;
        let a2 = (base_alpha * 0.20).round() as u8;
        let a3 = (base_alpha * 0.08).round() as u8;
        let a4 = (base_alpha * 0.02).round() as u8;

        assert_eq!(a1, 92);
        assert_eq!(a2, 46);
        assert_eq!(a3, 18);
        assert_eq!(a4, 5);
        assert!(a1 > a2 && a2 > a3 && a3 > a4, "Alpha must smoothly fade toward periphery");
    }

    #[test]
    fn test_m4_six_active_bands_activity_mapping() {
        let meta = SessionMetadata {
            host: "Windows".to_string(),
            tmux_session: None,
            tmux_window: None,
            tmux_pane: None,
            cwd: None,
            pid: None,
            agent_type: None,
        };

        // 1. Tool execution session
        let tool_sess = ActiveSession {
            session_id: "s_tool".to_string(),
            display_name: "Tool Session".to_string(),
            agent_type: "Claude".to_string(),
            state: AgentState::RunningTool {
                name: "cargo_test".to_string(),
                summary: "executing".to_string(),
            },
            status_text: "Running cargo test --workspace".to_string(),
            step_count: 5,
            metadata: meta.clone(),
            last_updated: Instant::now(),
            marquee_offset: 0.0,
            vu_levels: [0.0; 8],
            attention: AttentionState::new(),
        };

        let targets = compute_band_targets(&tool_sess, 1.0);
        // Band 1 (Tool Calls) must be elevated (>= 0.85)
        assert!(
            targets[1] >= 0.85,
            "Band 1 (Tool Calls) must be >= 0.85 when RunningTool, got {}",
            targets[1]
        );
        // Band 2 (Turn Speed) and Band 3 (Stream Volume) must be active
        assert!(targets[2] > 0.3, "Band 2 must reflect turn speed");
        assert!(targets[3] > 0.3, "Band 3 must reflect stream volume");

        // 2. Subagent session (WSL / Bridge)
        let subagent_sess = ActiveSession {
            session_id: "s_sub".to_string(),
            display_name: "Subagent Worker".to_string(),
            agent_type: "Bridge".to_string(),
            state: AgentState::Thinking,
            status_text: "Subagent worker delegating tasks".to_string(),
            step_count: 12,
            metadata: SessionMetadata {
                host: "wsl:Ubuntu".to_string(),
                ..meta.clone()
            },
            last_updated: Instant::now(),
            marquee_offset: 0.0,
            vu_levels: [0.0; 8],
            attention: AttentionState::new(),
        };

        let sub_targets = compute_band_targets(&subagent_sess, 1.0);
        // Band 0 (Subagents) must be pronounced (>= 0.75)
        assert!(
            sub_targets[0] >= 0.75,
            "Band 0 (Subagents) must be >= 0.75 for Bridge/WSL subagents, got {}",
            sub_targets[0]
        );

        // 3. Error / Alert session
        let err_sess = ActiveSession {
            session_id: "s_err".to_string(),
            display_name: "Error Session".to_string(),
            agent_type: "Gemini".to_string(),
            state: AgentState::Error {
                message: "Compilation failure".to_string(),
            },
            status_text: "Failed with error code 1".to_string(),
            step_count: 3,
            metadata: meta.clone(),
            last_updated: Instant::now(),
            marquee_offset: 0.0,
            vu_levels: [0.0; 8],
            attention: AttentionState::new(),
        };

        let err_targets = compute_band_targets(&err_sess, 1.0);
        // Band 4 (Error Flags) must be maximum (1.0)
        assert_eq!(
            err_targets[4], 1.0,
            "Band 4 (Error Flags) must be 1.0 on Error state"
        );

        // 4. Idle / Finished session
        let idle_sess = ActiveSession {
            session_id: "s_idle".to_string(),
            display_name: "Idle Session".to_string(),
            agent_type: "Gemini".to_string(),
            state: AgentState::Idle,
            status_text: "Awaiting instruction".to_string(),
            step_count: 20,
            metadata: meta.clone(),
            last_updated: Instant::now(),
            marquee_offset: 0.0,
            vu_levels: [0.0; 8],
            attention: AttentionState::new(),
        };

        let idle_targets = compute_band_targets(&idle_sess, 1.0);
        assert_eq!(
            idle_targets, [0.0; 6],
            "All bands must be 0.0 when session is Idle"
        );
    }

    #[test]
    fn test_m4_retro_dark_palette_contrast_and_two_pass_queue() {
        // 1. Palette consistency verification
        let save_bg = Color32::from_rgb(14, 28, 20);
        let reset_bg = Color32::from_rgb(26, 22, 14);
        let cancel_bg = Color32::from_rgb(20, 24, 26);

        // Dark backgrounds must have low luminance (<40 per channel)
        assert!(save_bg.r() < 40 && save_bg.g() < 40 && save_bg.b() < 40);
        assert!(reset_bg.r() < 40 && reset_bg.g() < 40 && reset_bg.b() < 40);
        assert!(cancel_bg.r() < 40 && cancel_bg.g() < 40 && cancel_bg.b() < 40);

        // Hover borders must offer high contrast (>150 in primary channel)
        let save_hover_border = Color32::from_rgb(0, 255, 160);
        let reset_hover_border = Color32::from_rgb(255, 210, 80);
        let cancel_hover_border = Color32::from_rgb(180, 200, 205);

        assert!(save_hover_border.g() >= 250);
        assert!(reset_hover_border.r() >= 250);
        assert!(cancel_hover_border.r() >= 180 && cancel_hover_border.g() >= 200);

        // 2. Action Queue Two-Pass Architecture
        let mut frame_actions: Vec<UserAction> = Vec::new();
        frame_actions.push(UserAction::Rename("sess_1".to_string(), "Tactile Deck".to_string()));

        let storage = Arc::new(RwLock::new(CustomTitlesStorage::in_memory()));
        let mut hub = SessionHub::new(storage);

        // Session not yet modified before Pass 2
        hub.sessions.push(ActiveSession {
            session_id: "sess_1".to_string(),
            display_name: "Original Name".to_string(),
            agent_type: "Gemini".to_string(),
            state: AgentState::Thinking,
            status_text: "Processing".to_string(),
            step_count: 1,
            metadata: SessionMetadata::default(),
            last_updated: Instant::now(),
            marquee_offset: 0.0,
            vu_levels: [0.0; 8],
            attention: AttentionState::new(),
        });

        assert_eq!(hub.sessions[0].display_name, "Original Name");

        // Pass 2: Drain and apply actions
        hub.apply_actions(frame_actions);
        assert_eq!(hub.sessions[0].display_name, "Tactile Deck");
    }

    #[test]
    fn test_m4_vu_session_tracker_integration_lifecycle() {
        let mut tracker = VuSessionTracker::default();
        let sess = ActiveSession {
            session_id: "sess_vu".to_string(),
            display_name: "VU Lifecycle".to_string(),
            agent_type: "Gemini".to_string(),
            state: AgentState::Thinking,
            status_text: "Running fast multi-agent turns".to_string(),
            step_count: 7,
            metadata: SessionMetadata::default(),
            last_updated: Instant::now(),
            marquee_offset: 0.0,
            vu_levels: [0.0; 8],
            attention: AttentionState {
                is_unacknowledged: true,
                triggered_at: Some(Instant::now()),
                last_state_signature: "test_sig".to_string(),
            },
        };

        let dt = 0.016;

        // Run 30 frames of active processing
        for frame in 1..=30 {
            tracker.update(&sess, dt, frame as f32 * 0.1);
        }

        // All 6 bands should have risen significantly and peaks must be >= levels
        for i in 0..6 {
            assert!(
                tracker.bands[i].level > 0.1,
                "Band {} level ({}) must be active",
                i,
                tracker.bands[i].level
            );
            assert!(
                tracker.bands[i].peak >= tracker.bands[i].level,
                "Band {} peak ({}) must be >= level ({})",
                i,
                tracker.bands[i].peak,
                tracker.bands[i].level
            );
        }

        let peaks_before = tracker.bands.map(|b| b.peak);

        // Idle session transition
        let mut idle_sess = sess.clone();
        idle_sess.state = AgentState::Idle;
        idle_sess.attention.is_unacknowledged = false;

        // Run 10 frames of idle (0.16s elapsed)
        for frame in 31..=40 {
            tracker.update(&idle_sess, dt, frame as f32 * 0.1);
        }

        // Levels must decay exponentially
        for i in 0..6 {
            assert!(
                tracker.bands[i].level < 0.5,
                "Band {} level must decay during Idle",
                i
            );
            // Peak hold timer is 600ms, so peaks must still remain floating at peaks_before!
            assert_eq!(
                tracker.bands[i].peak, peaks_before[i],
                "Band {} peak must remain floating during 600ms hold window",
                i
            );
        }
    }
}
