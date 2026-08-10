#!/usr/bin/env bash
# cargo-env.sh — MSVC build environment for Hermix Desktop (Git Bash).
#
# Problem 1: in Git Bash, Git ships its own Unix `link` (a hardlink tool) that
# shadows MSVC's `link.exe` on PATH, producing opaque "LNK1181 / extra operand"
# linker errors during cargo builds.
# Fix 1: prepend the MSVC bin dir so bare `link` resolves to MSVC's linker.
# Rust linking itself uses rust-lld.exe (configured in .cargo/config.toml)
# with explicit LIBPATH rustflags, so no LIB/INCLUDE env vars are required.
#
# Problem 2: build artifacts scattered across multiple target-dirs when
# running `cargo` (from repo root) vs `cargo tauri build` (which resolves
# relative target-dir from src-tauri/ manifest, going 4 levels up outside
# the repo). Hardcoding an absolute path in .cargo/config.toml would lock
# the project to one machine location — wrong for a shared repo.
# Fix 2: set CARGO_TARGET_DIR via this script. SCRIPT_DIR is computed from
# the script's own location, so wherever the repo lives, the target dir
# follows. CARGO_TARGET_DIR outranks .cargo/config.toml's [build].target-dir
# (cargo spec), and tauri-build's cargo subprocess inherits env vars from
# the parent — so CLI builds, desktop exe, and NSIS bundle all land in
# the same place: <repo-root>/release/target/release/.
#
# **MANDATORY**: source this BEFORE any cargo command:
#   . ./cargo-env.sh && cargo check --workspace --all-targets
# or via the CI gate:
#   . ./cargo-env.sh && ./scripts/ci-gate.sh
# Without sourcing, builds fall back to .cargo/config.toml's relative
# `release/target` (which splits between cargo and tauri-build), the very
# bug this script exists to prevent.

MSVC_VER="14.44.35207"
MSVC_BIN="/c/Program Files/Microsoft Visual Studio/2022/Community/VC/Tools/MSVC/${MSVC_VER}/bin/Hostx64/x64"

if [ -d "$MSVC_BIN" ]; then
    export PATH="${MSVC_BIN}:$PATH"
    echo "[cargo-env] prepended MSVC bin to PATH: ${MSVC_BIN}"
else
    echo "[cargo-env][warn] MSVC bin dir not found: ${MSVC_BIN}" >&2
fi

# 统一编译产物位置：脚本所在目录解析，跟随仓库走（不锁绝对路径）
# pwd -W 给 Windows 风格路径 (<repo>), cargo 看得懂;
# 普通 pwd 给 POSIX 风格 (/f/spiker/...), cargo 会误解成 drive:F:
# 下的 /f/... 子路径, target_dir 变成 F:/f/spiker/hermix-desktop/...
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -W)"
export CARGO_TARGET_DIR="${SCRIPT_DIR}/release/target"
echo "[cargo-env] CARGO_TARGET_DIR=${CARGO_TARGET_DIR}"
