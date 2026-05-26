# battcurve command runner
# https://github.com/casey/just

# Default: list recipes
default:
    @just --list

# Build (debug)
build:
    cargo build

# Build optimized release binary
release:
    cargo build --release

# Install the `battcurve` binary to ~/.cargo/bin (must be on your PATH)
install:
    cargo install --path .
    @echo "Installed. Run 'battcurve --help' (ensure ~/.cargo/bin is on your PATH)."

# Run all tests
test:
    cargo test

# Lint with clippy (warnings as errors)
lint:
    cargo clippy --all-targets -- -D warnings

# Format the code
fmt:
    cargo fmt

# Background logger; default interval 10s. Usage: just run-log 5s
run-log interval="10s":
    cargo run -- log --interval "$(echo '{{interval}}' | sed 's/^interval=//')"

# One-shot capture session (Ctrl-C to stop and print summary)
capture:
    cargo run -- capture

# htop-style live TUI monitor
run-tui:
    cargo run -- tui

# Local web UI with analysis charts. Usage: just serve 8787
serve port="8787":
    cargo run -- serve --port "$(echo '{{port}}' | sed 's/^port=//')"

# Print resolved data paths
paths:
    cargo run -- paths

# Install + start the systemd --user background logger
install-service: release
    mkdir -p ~/.config/systemd/user
    install -m644 systemd/battcurve-logger.service ~/.config/systemd/user/
    install -m755 target/release/battcurve ~/.local/bin/battcurve
    systemctl --user daemon-reload
    systemctl --user enable --now battcurve-logger.service
    @echo "Logger running. Check: systemctl --user status battcurve-logger"
