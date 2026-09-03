# 🎛️ Agent Deck (`agent-deck`)

A retro **Winamp-inspired** floating desktop activity deck and multi-stream session monitor for AI coding assistants (**Gemini / Antigravity**, **Claude Code**) running natively on **Windows** and inside **WSL2** (including native **`tmux`** session tracking).

```
┌──────────────────────────────────────────────────────────────────[─][^][X]┐
│ 🎛️ AGENT-DECK v0.3                                     [A-][A+]          │
├───────────────────────────────────────────────────────────────────────────┤
│   o Windows • 2       * clibox • 1                                        │
├───────────────────────────────────────────────────────────────────────────┤
│ ┌─ Gemini • clibox • tmux:backend:0.1 [EDIT] ──────── STEP 050 ─[▂▃▅▆]──┐ │
│ │  INPUT REQUIRED: Confirm running migrations on postgres-dev [Y/n]     │ │
│ └───────────────────────────────────────────────────────────────────────┘ │
│ ┌─ Gemini • Activity Bridge Daemon Plan [EDIT] ────── STEP 074 ─[ ▂▃ ]──┐ │
│ │  TOOL replace_file_content: Updating main.rs                          │ │
│ └───────────────────────────────────────────────────────────────────────┘ │
├───────────────────────────────────────────────────────────────────────────┤
│ * 3 active • 1 requiring input       [EDIT] Rename • Drag corner to resize│
└───────────────────────────────────────────────────────────────────────────┘
```

---

## ✨ Features

- **Dynamic Environment Self-Detection & Zero-Session Filtering**:
  - Automatically discovers connected host environments (`Windows`, `clibox`, `ubuntu`, etc.) from live session streams.
  - **Auto-Hide on 0 Sessions**: If an environment has no active sessions, its tab is hidden to keep the interface clean and clutter-free (while `Windows` remains permanently visible as the default anchor).
- **Thick Tactile Session Rows**:
  - **Status LED with Halo**:
    - 🟢 **Solid Emerald**: Thinking / Reasoning.
    - 🟢 **Solid Green**: Tool execution (`replace_file_content`, `cargo_test`).
    - 🟡 **Steady Solid Amber**: Action Required (waiting on user prompt or confirmation).
    - 🔵 **Cyan**: Finished / Ready for review.
    - 🔴 **Red**: Error / crashed.
  - **`tmux` Metadata Capture**: Automatically queries `tmux list-panes` and tags active tmux sessions (`tmux:backend:0.1`).
  - **1-Line Ticker Display**: Smooth scrolling horizontal marquee for active tool execution, which **automatically stops completely** when input is required for instant readability.
  - **Per-Session VU Equalizer**: Dancing 6-segment LED bars visualizing live activity and streaming flux.
- **Smart Title Resolution & Persistent Rename UI**:
  - Resolves semantic titles using a priority hierarchy:
    1. **User Custom Overwrite** (Persisted across process restarts in `session_titles.json`).
    2. **Earliest Markdown `# Heading 1`** in `brain/<uuid>/*.md`.
    3. **Workspace Basename** (e.g. `agent-deck`).
  - Click **`[EDIT]`** on any row to rename with instant real-time update and <kbd>Enter</kbd>-to-save.
- **Floating & Fully Reactive Window**:
  - Always-on-top, frameless, draggable chassis.
  - Interactive font zoom controls (**`A+` / `A-`**).
  - Tactile resize corner grip with responsive vertical scroll area that automatically reveals more rows as you expand the height.
  - Windowshade mini-mode (`_ / ^`).

---

## 🏗️ Architecture

Organized as a modular **Cargo Workspace**:

```
agent-deck/
├── Cargo.toml               # Workspace Coordinator
├── Makefile                 # Unified Workspace Dev Commands
├── AgentDeck.exe            # Standalone Windows Release Binary
├── Launch.bat               # One-click Windows Launcher
│
└── crates/
    ├── agent-deck-core/     # 📦 Shared Domain Protocol (AgentState, SessionEvent, SessionMetadata)
    ├── agent-deck-ui/       # 🖥️ Windows GUI (Rust + eframe/egui + Stream Adapters)
    └── agent-deck-daemon/   # 🐧 WSL2 Linux Bridge Daemon (tokio + tmux inspector + transcript watcher)
```

---

## 🚀 Quick Start & Development

### Common Dev Commands (`Makefile`)

| Command | Description |
| :--- | :--- |
| **`make run-win`** | Builds and launches the Windows floating desktop widget |
| **`make build`** | Builds release binaries across the workspace on Windows |
| **`make dev-wsl`** | Runs `agent-deck-daemon` in your default WSL distro (`clibox`) using fast `/tmp` native cache |
| **`make dev-wsl WSL_DISTRO=<name>`** | Runs the daemon in any specific WSL instance (e.g. `ubuntu-24.04`) |
| **`make install-wsl`** | Permanently installs the daemon into `~/.cargo/bin` in WSL |
| **`make clean`** | Cleans cargo build caches |

---

## 📄 License

MIT
