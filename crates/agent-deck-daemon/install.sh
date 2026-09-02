#!/usr/bin/env bash
set -e

echo "?? Building Agent Deck WSL2 Bridge Daemon..."
cargo build --release -p agent-deck-daemon

mkdir -p ~/.local/bin
cp target/release/agent-deck-daemon ~/.local/bin/

echo "? Installed to ~/.local/bin/agent-deck-daemon"
echo "?? To run:"
echo "   agent-deck-daemon"
