# 🎛️ Agent Deck (`agent-deck`)

A retro **Winamp / Cyberdeck** inspired floating activity deck & status ticker for AI coding sessions (Antigravity `agy`, `Claude Code`, `Codex`).

Designed to float seamlessly as an **always-on-top, frameless desktop widget** on Windows, showing real-time agent status, tool calls, thinking states, and pulsing alert LEDs whenever an agent is waiting for your terminal input.

---

## 📸 Core Features & Aesthetic

- **Retro Winamp Chassis**: Beveled dark-metal enclosure with screw rivets and tactile preset channel buttons (`[CH-1: AGY LIVE]`, `[CH-2: CLAUDE]`, `[CH-3: CODEX]`).
- **Phosphor Green / Amber Dot Matrix LCD**: Continuous 1-line marquee ticker displaying current actions, tool executions (`grep_search`, `replace_file_content`), and prompts.
- **Dynamic VU Meter Equalizer**: Segmented LED audio-style bars visualizing agent activity / token flux in real time.
- **Pulsing LED Status Lights**:
  - 🟢 **Solid Green**: Agent actively thinking / executing tools.
  - 🟡 **Pulsing / Blinking Amber**: **Action Required** (waiting on user prompt, permission confirmation, or `[y/n]`).
  - 🔵 **Cyan / Cool Blue**: Finished / Idle.
  - 🔴 **Red**: Error / crash.
- **Window Management**: Draggable from anywhere on the deck, mini windowshade toggle (`▲/▼`), frameless and always-on-top.

---

## 🗺️ Roadmap & Stages

### ✅ Stage 1: Mock Simulation Mode
- Toggle `[SIM: ON / SIM: OFF]` right from the deck header.
- Automatically cycles realistic scenarios (Claude running unit tests, Codex waiting on permissions, tool executions) to dial in the look, feel, and animation dynamics.

### ✅ Stage 2: Windows Native Terminal / AGY Integration
- Background thread monitoring `$HOME/.gemini/antigravity-cli/brain/` session transcripts (`transcript.jsonl`).
- Live extraction of current tool calls (`toolSummary`, `toolAction`), step counts, and input wait states directly from native `agy` CLI executions.

### ⏳ Stage 3: WSL2 Bridge (Isolated Daemon)
- Lightweight daemon inside WSL2 (streaming events over `localhost` WebSocket/SSE or shared socket) to feed Linux-native Claude/Codex/AGY sessions directly into the Windows floating deck.

---

## 🚀 Running the Deck

```powershell
cd agent-deck-ui
cargo run --release
```
