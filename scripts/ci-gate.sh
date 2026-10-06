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
#   3b. Relay service tests (apps/relay, node --test)
#   4. Frontend types check (pnpm tsc --noEmit)
#   4b. Hermix M protocol tests + H5 build (apps/hm)
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
# Hard gates (exit non-zero on failure): 1, 2, 3, 3b, 4b, 6, 8, 9, 10, 11
# Soft gates (warn but continue): 4, 5, 7, 12
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
echo "=== [1/12] cargo check --workspace --all-targets ==="
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
echo "=== [2/12] clippy await_holding_lock guard ==="
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
echo "=== [3/12] cargo test --workspace --lib ==="
cargo test --workspace --lib
echo ""

#-----------------------------------------------------------------------
# 3b. Relay service tests
#     apps/relay is a Node service in this repo, not an external
#     dependency — it carries the phone-side auth, frame routing and
#     SQLite audit log, so its tests belong in the same gate as the Rust
#     they serve. Hard gate: a routing bug here strands every paired
#     device silently.
#-----------------------------------------------------------------------
echo "=== [3b/12] relay tests (node --test) ==="
if [ -d apps/relay ]; then
  if [ ! -d apps/relay/node_modules ]; then
    echo "  FAIL: apps/relay/node_modules missing — run 'npm install' in apps/relay"
    exit 1
  fi
  (cd apps/relay && npm test)
else
  echo "  SKIP: apps/relay not found"
fi
echo ""

#-----------------------------------------------------------------------
# 4. Frontend type check (soft gate — pre-existing errors are common)
#-----------------------------------------------------------------------
echo "=== [4/12] tsc --noEmit (frontend) ==="
if [ -d apps/desktop-tauri ]; then
  # 直接调 tsc 二进制，**不要**用 `pnpm tsc --noEmit`：后者不执行类型检查，
  # 只跑 pnpm 自己的 install 流程就返回 0（~0.8s、输出 "Already up to date"）。
  # 这道关卡用错命令时永远不会红——一道不会失败的门禁比没有门禁更糟，
  # 因为它给出虚假的安全感。`node_modules` 缺失时明确 SKIP，不静默通过。
  TSC=apps/desktop-tauri/node_modules/.bin/tsc
  if [ ! -x "$TSC" ]; then
    echo "  SKIP: $TSC not found (run 'pnpm install' in apps/desktop-tauri)"
  elif (cd apps/desktop-tauri && ./node_modules/.bin/tsc --noEmit); then
    echo "  OK: frontend type check clean"
  else
    echo "  WARNING: frontend tsc failed (may be pre-existing — check output above)"
  fi
else
  echo "  SKIP: apps/desktop-tauri not found"
fi
echo ""

#-----------------------------------------------------------------------
# 4b. Hermix M (apps/hm) — protocol tests + H5 build
#     The companion app signs every request and frames every relay
#     message; both were built black-box against a spec. `build:h5` is
#     the only automated compile we can run here (no JDK / Android SDK
#     on this machine), so it is a hard gate on its own — a template
#     syntax error must not be able to reach a phone. The mobile-relay
#     decision note explicitly refuses "shipped on local build only".
#-----------------------------------------------------------------------
echo "=== [4b/12] hermix-m: typecheck + protocol tests + H5 build ==="
if [ -d apps/hm ]; then
  if [ ! -d apps/hm/node_modules ]; then
    echo "  FAIL: apps/hm/node_modules missing — run 'npm install' in apps/hm"
    exit 1
  fi
  (cd apps/hm && npm run typecheck && npm test && npm run build:h5)
else
  echo "  SKIP: apps/hm not found"
fi
echo ""

#-----------------------------------------------------------------------
# 5. Advisory: std::thread::sleep in non-test source
#    A strong signal of blocking-on-async-runtime. Not every hit is a
#    bug (sync retry loops are legitimate), but each should be reviewed
#    to confirm it's NOT on an async path. When a sleep IS needed in
#    async code, use tokio::time::sleep instead.
#-----------------------------------------------------------------------
echo "=== [5/12] advisory: std::thread::sleep in non-test src ==="
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
echo "=== [6/12] theme token guard ==="
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
# 7. God-file guard.
#    Flags single .rs/.ts/.tsx source files exceeding 2500 lines.
#
#    Baseline ratchet, not a global quota: the repo already carries 6
#    such files (see AGENTS.md "Code simplification discipline"), and
#    blocking on them outright would make the gate permanently red and
#    therefore ignored. Instead KNOWN_GOD_FILES lists the accepted
#    baseline — a file NOT on that list crossing 2500 lines FAILS, and
#    removing an entry from the list (by splitting the file) is the way
#    to tighten it. New god files cannot be introduced.
#-----------------------------------------------------------------------
echo "=== [7/12] god-file guard (> 2500 lines) ==="
# Accepted baseline. Remove a line only after actually splitting the file.
KNOWN_GOD_FILES="crates/hermix-agent/src/local_agent_service.rs
crates/hermix-agent/src/provider.rs
crates/hermix-agent/src/run_loop.rs
crates/hermix-agent/src/agent_loop/tests.rs
crates/hermix-server/src/ws.rs
apps/desktop-tauri/src/App.tsx"
over_limit=$(find crates apps/desktop-tauri/src -type f \( -name '*.rs' -o -name '*.ts' -o -name '*.tsx' \) \
  -exec wc -l {} + 2>/dev/null | awk '$1 > 2500 && $2 != "total" {print $2}' | sort)
new_gods=""
for f in $over_limit; do
  if ! printf '%s\n' "$KNOWN_GOD_FILES" | grep -qxF "$f"; then
    new_gods="$new_gods $f"
  fi
done
if [ -n "$new_gods" ]; then
  echo "  FAIL: new god file(s) over 2500 lines (not on the accepted baseline):"
  for f in $new_gods; do
    echo "    $(wc -l < "$f")  $f"
  done
  echo ""
  echo "  Split it, or — if it replaces an existing god file — update"
  echo "  KNOWN_GOD_FILES in scripts/ci-gate.sh in the same commit."
  exit 1
fi
if [ -n "$over_limit" ]; then
  echo "  OK: only the accepted baseline exceeds 2500 lines:"
  for f in $over_limit; do echo "    $(wc -l < "$f")  $f"; done
else
  echo "  (none over 2500 lines — clean)"
fi
echo ""

#-----------------------------------------------------------------------
# 7b. Ignored-test guard.
#     `#[ignore]` silently removes a test from `cargo test` — the suite
#     still reports green. This surfaces the count and fails on NEW
#     ignores so a failing test can't be parked indefinitely.
#-----------------------------------------------------------------------
echo "=== [7b/12] ignored test count ==="
IGNORED_TOTAL=$(grep -rn '#\[ignore' crates/ --include='*.rs' 2>/dev/null | wc -l | tr -d ' ')
IGNORED_BASELINE=17
if [ "$IGNORED_TOTAL" -gt "$IGNORED_BASELINE" ]; then
  echo "  FAIL: $IGNORED_TOTAL ignored tests (baseline $IGNORED_BASELINE)."
  echo "  New #[ignore] hides a test from CI. Fix the test or, if it must"
  echo "  stay ignored, raise IGNORED_BASELINE deliberately:"
  grep -rn '#\[ignore' crates/ --include='*.rs' 2>/dev/null | sed 's/^/    /'
  exit 1
fi
echo "  OK: $IGNORED_TOTAL ignored (baseline $IGNORED_BASELINE)"
echo ""

echo "=== [8/12] pre-existing debt markers ==="
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
echo "=== [9/12] skill freshness guard ==="
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

#-----------------------------------------------------------------------
# 10. Decision Notes structure gate (HARD gate)
#     docs/notes/ 三态决策记录的结构/格式/强制节检查（制度见 docs/notes/README.md）
#-----------------------------------------------------------------------
echo "=== [10/12] decision notes structure ==="
if [ -f scripts/check-notes.sh ]; then
  bash scripts/check-notes.sh || exit 1
else
  echo "  SKIP: scripts/check-notes.sh not found"
fi
echo ""

#-----------------------------------------------------------------------
# 11. Repository hygiene gate (HARD gate)
#     仓库根散件 + 新建顶层目录。check-notes.sh 管 notes 树的结构，
#     这一关管 agent 实际会弄脏的地方：仓库根和顶层目录列表。
#     只看已跟踪文件 —— 未跟踪的本地临时文件是开发者的事，脏工作区
#     不该让门禁变红。
#-----------------------------------------------------------------------
echo "=== [11/12] repository hygiene ==="
if [ -f scripts/check-hygiene.sh ]; then
  bash scripts/check-hygiene.sh || exit 1
else
  echo "  SKIP: scripts/check-hygiene.sh not found"
fi
echo ""

#-----------------------------------------------------------------------
# 12. Sidecar freshness gate (SOFT gate — warn, never block)
#     打包用的 sidecar 二进制可能比源码旧 —— cargo tauri build 不会自动
#     刷新它。2026-10-06 的 v2.0 缺陷报告就是这么把 5 个已修 bug 报成
#     "仍未修复"：复测跑的二进制比修复提交早 4 小时。这一关只提醒。
#-----------------------------------------------------------------------
echo "=== [12/12] sidecar freshness (soft) ==="
if [ -f scripts/check-sidecar-freshness.sh ]; then
  bash scripts/check-sidecar-freshness.sh || echo "  WARN: sidecar is stale — a retest through the packaged app would measure old code"
else
  echo "  SKIP: scripts/check-sidecar-freshness.sh not found"
fi
echo ""

echo "============================================"
echo "  CI Gate complete"
echo "============================================"
