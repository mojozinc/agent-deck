use std::process::Command;

#[derive(Clone, Debug)]
pub struct TmuxPaneInfo {
    pub pane_pid: u32,
    pub session_name: String,
    pub window_index: String,
    pub pane_index: String,
    pub window_name: String,
    pub current_path: String,
}

pub struct TmuxInspector;

impl TmuxInspector {
    /// Queries the active tmux server for all running sessions and panes
    pub fn list_all_panes() -> Vec<TmuxPaneInfo> {
        let output = match Command::new("tmux")
            .args([
                "list-panes",
                "-a",
                "-F",
                "#{pane_pid} #{session_name} #{window_index} #{pane_index} #{window_name} #{pane_current_path}",
            ])
            .output()
        {
            Ok(out) if out.status.success() => out,
            _ => return Vec::new(),
        };

        let text = String::from_utf8_lossy(&output.stdout);
        let mut panes = Vec::new();

        for line in text.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 5 {
                let pane_pid = parts[0].parse::<u32>().unwrap_or(0);
                let session_name = parts[1].to_string();
                let window_index = parts[2].to_string();
                let pane_index = parts[3].to_string();
                let window_name = parts[4].to_string();
                let current_path = if parts.len() >= 6 {
                    parts[5..].join(" ")
                } else {
                    String::new()
                };

                panes.push(TmuxPaneInfo {
                    pane_pid,
                    session_name,
                    window_index,
                    pane_index,
                    window_name,
                    current_path,
                });
            }
        }

        panes
    }

    /// Finds the tmux session/window info corresponding to a working directory or PID
    pub fn resolve_metadata(cwd: Option<&str>, pid: Option<u32>) -> Option<(String, String, String)> {
        let panes = Self::list_all_panes();
        if panes.is_empty() {
            return None;
        }

        // 1. Try matching by exact PID if provided
        if let Some(target_pid) = pid {
            if let Some(pane) = panes.iter().find(|p| p.pane_pid == target_pid) {
                return Some((
                    pane.session_name.clone(),
                    format!("{}:{}", pane.window_index, pane.window_name),
                    format!("%{}", pane.pane_index),
                ));
            }
        }

        // 2. Try matching by working directory
        if let Some(target_cwd) = cwd {
            if let Some(pane) = panes.iter().find(|p| !p.current_path.is_empty() && target_cwd.starts_with(&p.current_path)) {
                return Some((
                    pane.session_name.clone(),
                    format!("{}:{}", pane.window_index, pane.window_name),
                    format!("%{}", pane.pane_index),
                ));
            }
        }

        // 3. If there is only one active tmux session, attribute to it
        if panes.len() == 1 {
            let pane = &panes[0];
            return Some((
                pane.session_name.clone(),
                format!("{}:{}", pane.window_index, pane.window_name),
                format!("%{}", pane.pane_index),
            ));
        }

        None
    }
}
