# ADR 0001: State Engine Differentiation, Frame Action Queue & Crash Forensics

* **Status**: Accepted
* **Date**: 2026-09-03
* **Author**: mojozinc
* **Context**: Agent Deck Architectural Polish & Bug Post-Mortem

---

## 1. Context & Problem Statement

During real-world dogfooding of the **Agent Deck** multi-stream session monitor, four critical architectural flaws were identified:

1. **Immediate-Mode Array Clobbering & Out-of-Bounds Panics**:
   * In `egui`, mutating the underlying `self.hub.sessions` vector (via `Vec::retain`) in the middle of a loop that subsequently wrote back state (`self.hub.sessions[idx] = session`) caused off-by-one index corruption. Dismissing row $N$ overwrote and destroyed row $N+1$, and clicking the last card triggered an out-of-bounds index panic.
2. **Ambiguous State Engine (`RunningTool` vs `WaitingForInput` vs `WaitingForApproval`)**:
   * When an agent was executing commands (e.g. `cargo test`, `run_command`), the transcript watcher classified `PLANNER_RESPONSE` with `status: DONE` as `WaitingForInput`. This triggered false `"INPUT REQUIRED"` alerts while the tool was actively running.
   * Interactive modal questions and permission checks (e.g. `ask_question`, migration confirmation) lacked a distinct `WaitingForApproval` state.
3. **Silent Crashes in Windows GUI Subsystem**:
   * Because `agent-deck-ui` is compiled with `#![windows_subsystem = "windows"]`, stdout/stderr are unattached. Any unhandled panic silently terminated the application without leaving a trace or error message.
4. **Static Title Resolution Lag**:
   * When a new session was created with only system initialization steps, temporary fallback titles (e.g. `Session a06e21`) were cached permanently and never upgraded once implementation plan markdown files or user prompts appeared.

---

## 2. Decision Drivers

* **Zero-Crash Stability**: Immediate-mode GUI loops must never perform in-place vector mutations that invalidate loop indexes.
* **Accurate Visual Status**: The deck must clearly distinguish between:
  * 🟢 **Active Tool Execution** (`RunningTool`): Green LED, scrolling args, dancing VU meters.
  * 🟠 **User Approval Required** (`WaitingForApproval`): Warning amber-orange glow, frozen ticker.
  * 🟡 **Turn Completed** (`WaitingForInput`): Steady amber LED, frozen ticker.
  * ⚪ **Stale Session** (`is_stale > 15m`): Muted slate styling with interactive `[DISMISS]`.
* **Crash Forensics**: All panics must write formatted stack traces and timestamps to `%APPDATA%\agent-deck\crash.log`.
* **Reactive Discovery**: New session directories and title updates must be detected dynamically.

---

## 3. Considered Options

* **Option 1 (In-Place Index Clamping)**: Keep mutating the vector in-place and attempt complex index recalculations.
  * *Rejected*: Brittle, prone to edge-case index out-of-bounds panics, and violates immediate-mode UI separation of concerns.
* **Option 2 (Two-Pass Frame Action Queue)**:
  * *Pass 1 (Read & Render)*: Draw visual components and emit user intents (`UserAction::Dismiss`, `UserAction::Rename`, `UserAction::Select`) into a frame action buffer.
  * *Pass 2 (State Transition)*: Apply all queued actions against `SessionHub` cleanly after the render pass finishes.
  * *Selected*: Industry standard for immediate-mode GUI architectures; completely eliminates array mutation collisions.

---

## 4. Decision & Architecture

### A. Two-Pass Action Queue Protocol
```rust
pub enum UserAction {
    Dismiss(String),
    Rename(String, String),
    Select(String),
    AcknowledgeCategory(String),
    AcknowledgeAll,
}
```
During rendering, widget callbacks append to `actions: Vec<UserAction>`. After the `ScrollArea` finishes, `hub.apply_actions(actions)` executes safe vector operations without index hazards.

### B. Extended Domain Protocol (`AgentState`)
```rust
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AgentState {
    Idle,
    Thinking,
    RunningTool { name: String, summary: String },
    WaitingForApproval { name: String, summary: String },
    WaitingForInput { prompt_preview: String },
    Error { message: String },
    Finished,
}
```

### C. Persistent Crash Logging Hook
On startup in `main()`:
```rust
std::panic::set_hook(Box::new(|panic_info| {
    let report = format_panic_report(panic_info, Backtrace::capture());
    let _ = std::fs::write(appdata_dir.join("crash.log"), &report);
}));
```

---

## 5. Consequences & Trade-offs

### Positive
- **Rock-Solid Stability**: Zero vector clobbering and zero out-of-bounds index panics on card dismissal or interaction.
- **High-Fidelity Status**: Developers immediately know if an agent is running a command vs waiting for input vs requiring modal approval.
- **Self-Healing Titles**: Newly created sessions dynamically update their friendly titles as plans and prompts arrive.
- **Forensic Visibility**: If an unhandled exception occurs, `%APPDATA%\agent-deck\crash.log` captures the exact stack trace.

### Negative / Trade-offs
- Slight allocation overhead of an action `Vec` per frame (negligible, typically 0–2 elements per frame).
