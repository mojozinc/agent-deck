# ==============================================================================
# Agent Deck Workspace Makefile
# ==============================================================================

# Configurable WSL Distro (Defaults to $WSL_DISTRO_NAME, or clibox if unset)
WSL_DISTRO ?= $(or $(WSL_DISTRO_NAME), clibox)

# Detect if make is executed natively in Linux or from Windows
ifeq ($(shell uname -s 2>/dev/null),Linux)
    WSL_EXEC := bash -c
else
    WSL_EXEC := wsl -d $(WSL_DISTRO) bash -c
endif

.PHONY: help build run-win dev-wsl install-wsl build-wsl clean

help:
	@echo "Agent Deck Dev Commands:"
	@echo "  make run-win             Launch the Windows desktop UI"
	@echo "  make build               Build release binaries on Windows"
	@echo "  make dev-wsl             Run WSL daemon in $(WSL_DISTRO) (fast /tmp cache)"
	@echo "  make install-wsl         Install daemon binary into ~/.cargo/bin in $(WSL_DISTRO)"
	@echo "  make build-wsl           Build daemon in $(WSL_DISTRO)"
	@echo "  make clean               Clean cargo build caches"
	@echo ""
	@echo "Override WSL Distro Example:"
	@echo "  make dev-wsl WSL_DISTRO=ubuntu-24.04"

build:
	cargo build --workspace --release

run-win:
	cargo run -p agent-deck-ui --release

dev-wsl:
	$(WSL_EXEC) 'export PATH="$$HOME/.cargo/bin:$$PATH"; cd /mnt/c/Users/schordinger/workbench/agent-deck && CARGO_TARGET_DIR=/tmp/target-agent-deck cargo run -p agent-deck-daemon'

install-wsl:
	$(WSL_EXEC) 'export PATH="$$HOME/.cargo/bin:$$PATH"; cd /mnt/c/Users/schordinger/workbench/agent-deck && cargo install --path crates/agent-deck-daemon --force'

build-wsl:
	$(WSL_EXEC) 'export PATH="$$HOME/.cargo/bin:$$PATH"; cd /mnt/c/Users/schordinger/workbench/agent-deck && CARGO_TARGET_DIR=/tmp/target-agent-deck cargo build -p agent-deck-daemon --release'

clean:
	cargo clean
