#!/usr/bin/env bash
# cargo-env.sh — MSVC build environment for Hermix Desktop (Git Bash).
#
# Problem: in Git Bash, Git ships its own Unix `link` (a hardlink tool) that
# shadows MSVC's `link.exe` on PATH, producing opaque "LNK1181 / extra operand"
# linker errors during cargo builds.
#
# Fix: prepend the MSVC bin dir so bare `link` resolves to MSVC's linker.
# Rust linking itself uses rust-lld.exe (configured in .cargo/config.toml)
# with explicit LIBPATH rustflags, so no LIB/INCLUDE env vars are required.
#
# Source this BEFORE any cargo command:
#   . ./cargo-env.sh && cargo check --workspace --all-targets
# or via the CI gate:
#   . ./cargo-env.sh && ./scripts/ci-gate.sh

MSVC_VER="14.44.35207"
MSVC_BIN="/c/Program Files/Microsoft Visual Studio/2022/Community/VC/Tools/MSVC/${MSVC_VER}/bin/Hostx64/x64"

if [ -d "$MSVC_BIN" ]; then
    export PATH="${MSVC_BIN}:$PATH"
    echo "[cargo-env] prepended MSVC bin to PATH: ${MSVC_BIN}"
else
    echo "[cargo-env][warn] MSVC bin dir not found: ${MSVC_BIN}" >&2
fi
