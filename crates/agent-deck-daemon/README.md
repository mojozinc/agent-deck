# 🐧 Agent Deck WSL2 Daemon (`agent-deck-daemon`)

The headless background bridge daemon for **WSL2** (and Linux containers). It observes local AI assistant transcripts, queries `tmux` for active pane/session metadata, and streams real-time status events to the Windows **Agent Deck** floating UI.

---

## ⚡ Key Capabilities

- **Automatic Distro Detection**: Automatically inspects `$WSL_DISTRO_NAME` (e.g. `clibox`, `Ubuntu-24.04`, `Debian`) and tags each session's environment metadata.
- **`tmux` Session & Window Inspection**: Uses `tmux list-panes` and PTY inspection to associate running agent processes with their exact tmux session (e.g. `tmux:backend:0.1`).
- **Transcript Event Streamer**: Non-intrusively monitors append-only logs in `~/.gemini/antigravity-cli/brain/` and `~/.claude/`.
- **TCP Broadcast Server**: Binds to `0.0.0.0:8765`, seamlessly reachable from Windows localhost at `127.0.0.1:8765`.

---

## 🚀 Quick Start & Development

### 1. Run Live in WSL from Windows (Fast /tmp ext4 Cache)
From the root repository on Windows:
```bash
# Runs inside default distro (clibox)
make dev-wsl

# Or specify any custom WSL distro:
make dev-wsl WSL_DISTRO=ubuntu-24.04
```

### 2. Install Permanently in WSL
```bash
make install-wsl
# Installs to ~/.cargo/bin/agent-deck-daemon in your WSL environment
```

### 3. Autostart via Systemd in WSL (Optional)
To run `agent-deck-daemon` continuously in the background whenever WSL starts:

1. Create `~/.config/systemd/user/agent-deck-daemon.service`:
   ```ini
   [Unit]
   Description=Agent Deck Activity Bridge Daemon
   After=network.target

   [Service]
   ExecStart=%h/.cargo/bin/agent-deck-daemon
   Restart=always
   RestartSec=3

   [Install]
   WantedBy=default.target
   ```
2. Enable and start:
   ```bash
   systemctl --user daemon-reload
   systemctl --user enable --now agent-deck-daemon
   ```
