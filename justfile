default:
    @just --list

# Run the GUI application
gui:
    cargo run -p inateck-bt-gatt-gui

# Run CLI with arguments (e.g. just cli scan, just cli read -a AB:2B:00:26:6D:23 -C)
cli *ARGS:
    cargo run -p inateck-bt-gatt-cli -- {{ARGS}}

# Build release binaries
build:
    cargo build --release

# Run clippy lints
lint:
    cargo clippy --workspace

# Format code
fmt:
    cargo fmt --all
