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
#   7. Advisory: source files > 2500 lines (god-file drift)
#   8. No `TODO: pre-existing` / `FIXME: pre-existing` debt markers
#      (see AGENTS.md "Conventions" — don't introduce them)
#   9. Skill freshness guard: no loaded skill doc may reference
#      decommissioned identifiers (`workspace_*` tool family, `@work`
#      client entry, `WorkspaceSchema`). Builtin skills deleted from the
#      repo stay seeded in `$HERMIX_HOME/skills` forever, so a stale doc
#      can outlive the code and hijack skill routing — this gate fails on
#      the drift instead of letting it silently mislead the agent.
#
# Hard gates (exit non-zero on failure): 1, 2, 3, 6, 8, 9
# Soft gates (warn but continue): 4, 5, 7
#
# Usage (from repo root, in any terminal):
#   ./scripts/ci-gate.sh
#
# Note: no env sourcing needed — the linker (rust-lld.exe + explicit
# LIBPATH rustflags) is configured in .cargo/config.toml, which also
# bypasses Git Bash's link.exe shadow.
set -euo pipefail

echo "============================================"
echo "  Hermix Desktop CI Gate"
echo "============================================"
echo ""

#-----------------------------------------------------------------------
# 1. Compile check
#-----------------------------------------------------------------------
echo "=== [1/9] cargo check --workspace --all-targets ==="
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
echo "=== [2/9] clippy await_holding_lock guard ==="
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
echo "=== [3/9] cargo test --workspace --lib ==="
cargo test --workspace --lib
echo ""

#-----------------------------------------------------------------------
# 4. Frontend type check (soft gate — pre-existing errors are common)
#-----------------------------------------------------------------------
echo "=== [4/9] pnpm tsc --noEmit (frontend) ==="
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
echo "=== [5/9] advisory: std::thread::sleep in non-test src ==="
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
echo "=== [6/9] theme token guard ==="
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

#-----------------------------------------------------------------------
# 7. Advisory: single-file line-count guard (soft gate)
#    Flags god-file regressions: any .rs/.ts/.tsx source file exceeding
#    2500 lines is a smell (review for extractable modules). Pure
#    warning — does not fail the gate, so incremental splits aren't
#    blocked, but surfaces drift back toward god files.
#    See AGENTS.md "Code simplification discipline" for the 100k-line goal.
#-----------------------------------------------------------------------
echo "=== [7/9] advisory: source files > 2500 lines ==="
over_limit=$(find crates apps/desktop-tauri/src -type f \( -name '*.rs' -o -name '*.ts' -o -name '*.tsx' \) \
  -exec wc -l {} + 2>/dev/null | awk '$1 > 2500 {print}' | sort -rn)
if [ -n "$over_limit" ]; then
  echo "  ADVISORY: the following files exceed 2500 lines (candidate god files):"
  echo "$over_limit" | while read -r n f; do echo "    $n  $f"; done
  echo ""
  echo "  Consider extracting leaf modules / moving #[cfg(test)] out (see code-analysis-report)."
else
  echo "  (none over 2500 lines — clean)"
fi
echo ""

echo "=== [8/9] pre-existing debt markers ==="
debt_hits=$(grep -rn -E '(TODO|FIXME): pre-existing' \
  crates/ apps/desktop-tauri/src/ \
  --include='*.rs' --include='*.ts' --include='*.tsx' \
  2>/dev/null || true)
if [ -n "$debt_hits" ]; then
  echo "  FAIL: pre-existing debt markers found — fix the defect instead of marking it:"
  echo "$debt_hits" | sed 's/^/    /'
  exit 1
fi
echo "  OK: no TODO/FIXME: pre-existing markers"
echo ""

#-----------------------------------------------------------------------
# 9. Skill freshness guard (HARD gate)
#    Scans every skill doc the agent can load — $HERMIX_HOME/skills, the
#    repo builtin-skills tree, and the dashboard skills — for identifiers
#    of decommissioned features. Seeded home copies of deleted builtin
#    skills are never pruned by the app, so they outlive the code; their
#    description usually matches the user's wording first and wins routing,
#    which is how a workbench conversation ended up calling tools that do
#    not exist. See scripts/verify-skill-freshness.mjs for the token list
#    and the (narrow) exception for the generated workbench SKILL.md, which
#    names those identifiers only to forbid them.
#-----------------------------------------------------------------------
echo "=== [9/9] skill freshness guard ==="
if [ -f scripts/verify-skill-freshness.mjs ]; then
  if command -v node >/dev/null 2>&1; then
    node scripts/verify-skill-freshness.mjs 1>&2 || exit 1
  else
    echo "  SKIP: node not on PATH (install Node 20+ to enable this gate)"
  fi
else
  echo "  SKIP: scripts/verify-skill-freshness.mjs not found"
fi
echo ""

echo "============================================"
echo "  CI Gate complete"
echo "============================================"
