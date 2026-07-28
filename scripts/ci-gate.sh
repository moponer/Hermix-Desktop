#!/usr/bin/env bash
#
# CI gate for Hermix Desktop
# (P0-6 / STABILITY-PLAN §2.1-2.2)
#
# Enforces:
#   1. Workspace compiles cleanly (cargo check)
#   2. No std::sync / parking_lot lock guards held across `.await`
#      (clippy::await_holding_lock — catches guaranteed deadlocks)
#   3. Unit tests pass (cargo test --workspace --lib)
#   4. Frontend types check (pnpm tsc --noEmit)
#   5. Advisory report: std::thread::sleep in non-test src (potential
#      async-runtime blocking — each hit needs human review)
#   6. Theme token static guard: no hardcoded #hex / rgb() in .tsx/.ts
#      outside the registered EXCEPTIONS list (see docs/design/THEME_SYSTEM.md)
#
# Hard gates (exit non-zero on failure): 1, 2, 3, 6
# Soft gates (warn but continue): 4, 5
#
# Usage (from repo root, in Git Bash):
#   . ./cargo-env.sh && ./scripts/ci-gate.sh
#
# Note: cargo-env.sh MUST be sourced first to strip Git's link.exe
# from PATH and set up the MSVC environment. Without it, linking
# fails with opaque errors.
set -euo pipefail

echo "============================================"
echo "  Hermix Desktop CI Gate"
echo "============================================"
echo ""

#-----------------------------------------------------------------------
# 1. Compile check
#-----------------------------------------------------------------------
echo "=== [1/6] cargo check --workspace --all-targets ==="
cargo check --workspace --all-targets
echo ""

#-----------------------------------------------------------------------
# 2. clippy await_holding_lock guard
#    Catches std::sync::MutexGuard / RwLockReadGuard / RwLockWriteGuard
#    (and parking_lot equivalents) held across .await — a guaranteed
#    deadlock because the owning thread is parked and can't release.
#
#    NOTE: tokio::sync locks are async-aware and NOT caught by this
#    lint. Those require code review (see P0-4 cdp.rs / P0-5 session.rs
#    for the patterns to avoid: nested locks, lock-held-during-long-read).
#-----------------------------------------------------------------------
echo "=== [2/6] clippy await_holding_lock guard ==="
if rustup component list --installed 2>/dev/null | grep -q '^clippy'; then
  cargo clippy --workspace --all-targets -- \
    -D clippy::await_holding_lock
  echo "  OK: no std::sync lock-across-await violations"
else
  echo "  SKIP: clippy component not installed (run 'rustup component add clippy')"
fi
echo ""

#-----------------------------------------------------------------------
# 3. Unit tests
#-----------------------------------------------------------------------
echo "=== [3/6] cargo test --workspace --lib ==="
cargo test --workspace --lib
echo ""

#-----------------------------------------------------------------------
# 4. Frontend type check (soft gate — pre-existing errors are common)
#-----------------------------------------------------------------------
echo "=== [4/6] pnpm tsc --noEmit (frontend) ==="
if [ -d apps/desktop-tauri ]; then
  (cd apps/desktop-tauri && pnpm tsc --noEmit) || {
    echo "  WARNING: frontend tsc failed (may be pre-existing — check output above)"
  }
else
  echo "  SKIP: apps/desktop-tauri not found"
fi
echo ""

#-----------------------------------------------------------------------
# 5. Advisory: std::thread::sleep in non-test source
#    A strong signal of blocking-on-async-runtime. Not every hit is a
#    bug (sync retry loops are legitimate), but each should be reviewed
#    to confirm it's NOT on an async path. When a sleep IS needed in
#    async code, use tokio::time::sleep instead.
#-----------------------------------------------------------------------
echo "=== [5/6] advisory: std::thread::sleep in non-test src ==="
sleep_hits=$(grep -rn 'std::thread::sleep' \
  crates/*/src/ --include='*.rs' \
  | grep -v 'mod tests' \
  | grep -v '#\[test\]' \
  | grep -v '#\[cfg(test)\]' \
  2>/dev/null || true)
if [ -n "$sleep_hits" ]; then
  echo "  ADVISORY: std::thread::sleep found in non-test src (review for async-blocking):"
  echo "$sleep_hits" | sed 's/^/    /'
  echo ""
  echo "  If any of these are on an async path, replace with tokio::time::sleep"
  echo "  or wrap the sync section in tokio::task::spawn_blocking."
else
  echo "  (none found)"
fi
echo ""

#-----------------------------------------------------------------------
# 6. Theme token static guard (HARD gate)
#    Scans .tsx/.ts for hardcoded color literals outside the
#    registered exceptions (see apps/desktop-tauri/src/styles/themes/EXCEPTIONS.md).
#    Enforced by docs/design/THEME_SYSTEM.md §5.
#
#    Need: node >= 18. The script uses no external deps.
#-----------------------------------------------------------------------
echo "=== [6/6] theme token guard ==="
if [ -f scripts/check-theme-tokens.mjs ]; then
  if command -v node >/dev/null 2>&1; then
    node scripts/check-theme-tokens.mjs 1>&2 || exit 1
  else
    echo "  SKIP: node not on PATH (install Node 20+ to enable this gate)"
  fi
else
  echo "  SKIP: scripts/check-theme-tokens.mjs not found"
fi
echo ""

echo "============================================"
echo "  CI Gate complete"
echo "============================================"
