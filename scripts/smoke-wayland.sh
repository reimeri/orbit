#!/usr/bin/env bash
set -euo pipefail
binary=${1:-./target/debug/orbit}
app_args=(--smoke-test)
output_args=()
if (( $# == 2 )); then
    app_args=(--marketing-shot "$2")
    output_args=(--width=1280 --height=800 --scale=2)
elif (( $# > 2 )); then
    echo "Usage: $0 [BINARY [MARKETING.png]]" >&2
    exit 2
fi
if [[ ! -x "$binary" ]]; then
    echo "Missing executable: $binary. Run 'cargo build --locked' inside nix develop first." >&2
    exit 1
fi
runtime=$(mktemp -d)
weston_pid=
cleanup() {
    if [[ -n "$weston_pid" ]]; then
        kill "$weston_pid" 2>/dev/null || true
        wait "$weston_pid" 2>/dev/null || true
    fi
    rm -rf "$runtime"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
export XDG_RUNTIME_DIR="$runtime"
export WAYLAND_DISPLAY=orbit-smoke
export ICED_BACKEND=wgpu
unset DISPLAY WAYLAND_SOCKET

weston --backend=headless --renderer=pixman --shell=kiosk "${output_args[@]}" \
    --socket="$WAYLAND_DISPLAY" --idle-time=0 --no-config \
    --log="$runtime/weston.log" >"$runtime/weston-stderr.log" 2>&1 &
weston_pid=$!
for ((attempt = 0; attempt < 100; attempt++)); do
    [[ -S "$runtime/$WAYLAND_DISPLAY" ]] && break
    kill -0 "$weston_pid" 2>/dev/null || break
    sleep 0.1
done
if [[ ! -S "$runtime/$WAYLAND_DISPLAY" ]]; then
    cat "$runtime/weston.log" "$runtime/weston-stderr.log" >&2
    echo "Headless Wayland did not start. Re-enter nix develop and retry 'just smoke'." >&2
    exit 1
fi
if ! bash "$(dirname "$0")/test-renderer.sh" timeout 60s "$binary" "${app_args[@]}" >"$runtime/app.log" 2>&1; then
    cat "$runtime/app.log" "$runtime/weston.log" >&2
    echo "Window capture failed or timed out (renderer: $ICED_BACKEND)." >&2
    exit 1
fi
cat "$runtime/app.log"
# Closing the window without producing a frame is not a successful smoke test.
if [[ ${app_args[0]} == --smoke-test ]] && ! grep -q '^SMOKE_OK:' "$runtime/app.log"; then
    echo "No rendered-frame confirmation received." >&2
    exit 1
fi
