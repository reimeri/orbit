# Run these recipes inside `nix develop`.
default:
    @just --list

# Fast feedback; does not link an executable.
check:
    cargo check --locked --workspace --all-targets

# UI-independent tests: no iced/wgpu compilation required.
test-core:
    cargo test --locked -p pico-core

test:
    cargo test --locked --workspace

# Memory safety and leaked module references, not just successful process exit.
test-quickjs-memory:
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER="./scripts/check-quickjs-memory.sh" cargo test --locked --workspace --test quickjs_lifetime -- --test-threads=1
    bash scripts/check-quickjs-memory.sh

fmt:
    cargo fmt --all

lint:
    cargo fmt --all --check
    cargo clippy --locked --workspace --all-targets -- -D warnings

run:
    cargo run --locked -p pico-evo

# Deterministic software-rendered window check; no desktop or GPU required.
smoke:
    cargo build --locked -p pico-evo
    ./scripts/smoke-wayland.sh

# Requires a running Wayland desktop and a working graphics driver; no fallback.
smoke-gpu:
    cargo build --locked -p pico-evo
    env -u DISPLAY ICED_BACKEND=wgpu timeout 60s ./target/debug/pico-evo --smoke-test

# Pre-commit checks. Not the edit/compile inner loop.
verify: lint test test-quickjs-memory smoke

# Generate a Cargo timing report without cleaning incremental artifacts.
timings:
    cargo build --locked --timings
