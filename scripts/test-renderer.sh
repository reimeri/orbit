#!/usr/bin/env bash
# Test-only CPU Vulkan driver. Never source this from the application wrapper.
set -euo pipefail
if [[ -z "${ORBIT_TEST_VULKAN_ICD:-}" || ! -f "$ORBIT_TEST_VULKAN_ICD" ]]; then
    echo "The pinned CPU Vulkan driver is unavailable. Re-enter nix develop, then retry just test or just smoke." >&2
    exit 1
fi
export VK_DRIVER_FILES="$ORBIT_TEST_VULKAN_ICD"
unset VK_ICD_FILENAMES
export WGPU_BACKEND=vulkan ICED_BACKEND=wgpu MESA_SHADER_CACHE_DISABLE=true
exec "$@"
