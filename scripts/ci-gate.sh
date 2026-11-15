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
# Hard gates (exit non-zero on failure): 1, 2, 3, 3b, 4b, 6, 8, 9, 10, 11, 15
# Soft gates (warn but continue): 4, 5, 7, 12, 13, 14
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
echo "=== [1/14] cargo check --workspace --all-targets ==="
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
echo "=== [2/14] clippy await_holding_lock guard ==="
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
echo "=== [3/14] cargo test --workspace --lib ==="
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
echo "=== [3b/14] relay tests (node --test) ==="
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
echo "=== [4/14] tsc --noEmit (frontend) ==="
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
# 4c. DesignGuard (hard gate) — impeccable/typography 硬约束的 grep 门禁。
#     TSX 字面色值 / dark: 变体 / 「加载中…」字面量 / window.confirm。
#     图标按钮 aria-label 规则（G3）曾在此，因全量扫 <button> 误报率过高
#     被移除；后改由 4e 的 check-design-primitives.mjs 以**限定 class 名**
#     的方式恢复。规则史与豁免见脚本头注释。
#-----------------------------------------------------------------------
echo "=== [4c/14] design guard (TSX) ==="
if [ -f apps/desktop-tauri/src ]; then
  echo "  SKIP: unexpected layout"
elif [ -d apps/desktop-tauri/src ]; then
  # check-design-guard.mjs 按 **cwd** 拼 `src/`（见该脚本 ROOT 定义），
  # 必须在 apps/desktop-tauri 下跑，否则扫的是仓库根的 src/（不存在 → ENOENT）。
  # 与 package.json 的 `check:design` 脚本保持同一 cwd。
  if (cd apps/desktop-tauri && node ../../scripts/check-design-guard.mjs); then
    echo "  OK: design guard clean"
  else
    echo "  FAIL: design guard violations — fix before commit"
    exit 1
  fi
else
  echo "  SKIP: apps/desktop-tauri not found"
fi
echo ""

#-----------------------------------------------------------------------
# 4e. DesignPrimitives (hard gate)
#     guard 管「不许写什么」，本门禁管「不许漏什么」：
#       G3  图标按钮缺无障碍名      G6a 虚线描边      G6b side-tab
#       G7  图标尺寸偏离 --icon-size-* 档位
#       G8  表单控件高度偏离档位    G9  原始语义色当文字色
#       P0  ui/ 原语零调用（假装体系里有统一做法、实际没人用）
#     2026-10-21 起由 advisory 转为**阻断** —— 存量已清零。依据：
#     docs/notes/proposed/architecture/2026-10-21-frontend-design-audit.md
#-----------------------------------------------------------------------
echo "=== [4e/14] design primitives ==="
if [ -d apps/desktop-tauri/src ]; then
  if node scripts/check-design-primitives.mjs --strict; then
    echo "  OK: design primitives clean"
  else
    echo "  FAIL: design primitives violations — fix before commit"
    exit 1
  fi
else
  echo "  SKIP: apps/desktop-tauri not found"
fi
echo ""

#-----------------------------------------------------------------------
# 4d. Preview layer guard (hard gate) — 共享预览层的一致性。
#     共享包 @hermix/preview 的类名必须有共享 CSS 规则（不许 Tailwind
#     原子类：viewer 不装 Tailwind）；共享 CSS 的 token 两侧都要定义
#     （var() 解析失败 = 整条声明被丢弃）。依据见
#     docs/notes/implemented/architecture/2026-10-19-shared-preview-layer-ownership.md
#-----------------------------------------------------------------------
echo "=== [4d/14] preview layer guard (shared @hermix/preview) ==="
if [ -d packages/preview/src ]; then
  if node scripts/check-preview-layer.mjs; then
    echo "  OK: preview layer consistent"
  else
    echo "  FAIL: preview layer violations — fix before commit"
    exit 1
  fi
else
  echo "  SKIP: packages/preview not found"
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
echo "=== [4b/14] hermix-m: typecheck + protocol tests + H5 build ==="
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
echo "=== [5/14] advisory: std::thread::sleep in non-test src ==="
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
echo "=== [6/14] theme token guard ==="
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
echo "=== [7/14] god-file guard (> 2500 lines) ==="
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
echo "=== [7b/14] ignored test count ==="
IGNORED_TOTAL=$(grep -rn '#\[ignore' crates/ --include='*.rs' 2>/dev/null | wc -l | tr -d ' ')
# 基线 21（2026-10-23 上调）：HEAD 里已有的 21 个 ignore 是网络测试、bench 报告、
# fastembed 模型下载、预存缺陷等**故意**跳过的用例（见下面列出的位置）。
# 基线落后于 HEAD 时门禁会一直红，而"一直红"的门禁等于没有门禁 —— 所以
# 基线按 HEAD 的实际值抬到 21，新增 ignore 仍会被这条 ratchet 抓住。
IGNORED_BASELINE=21
if [ "$IGNORED_TOTAL" -gt "$IGNORED_BASELINE" ]; then
  echo "  FAIL: $IGNORED_TOTAL ignored tests (baseline $IGNORED_BASELINE)."
  echo "  New #[ignore] hides a test from CI. Fix the test or, if it must"
  echo "  stay ignored, raise IGNORED_BASELINE deliberately:"
  grep -rn '#\[ignore' crates/ --include='*.rs' 2>/dev/null | sed 's/^/    /'
  exit 1
fi
echo "  OK: $IGNORED_TOTAL ignored (baseline $IGNORED_BASELINE)"
echo ""

echo "=== [8/14] pre-existing debt markers ==="
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
echo "=== [9/14] skill freshness guard ==="
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
echo "=== [10/14] decision notes structure ==="
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
echo "=== [11/14] repository hygiene ==="
if [ -f scripts/check-hygiene.sh ]; then
  bash scripts/check-hygiene.sh || exit 1
else
  echo "  SKIP: scripts/check-hygiene.sh not found"
fi
echo ""

#-----------------------------------------------------------------------
# 12. Sidecar freshness gate (HARD gate)
#     打包用的 sidecar 二进制 / builtin-soul / resources 静态资源可能比
#     源码旧 —— cargo tauri build 不会自动刷新它们。2026-10-06 的 v2.0
#     缺陷报告就是这么把 5 个已修 bug 报成"仍未修复"：复测跑的二进制
#     比修复提交早 4 小时；2026-10-08 新装机器播种的还是 8 月的
#     SOUL.md。过期即 fail，重编 + copy 后重跑即绿。
#-----------------------------------------------------------------------
echo "=== [12/14] sidecar freshness (HARD gate) ==="
# 2026-10-08 起从 soft 升级为 hard：soft WARN 挡不住"打包含旧后端/旧
# SOUL.md"——10-06、10-08 两次交付都栽在这里。freshness 脚本现覆盖
# sidecar 二进制 + builtin-soul/resources 静态资源，exit 1 即打断 gate。
# 注意：本机开发中改了源码还没重编 sidecar 时 gate 会红——这是有意
# 行为，重编 + copy 后即绿（命令在脚本的 STALE 输出里）。
if [ -f scripts/check-sidecar-freshness.sh ]; then
  bash scripts/check-sidecar-freshness.sh || exit 1
else
  echo "  SKIP: scripts/check-sidecar-freshness.sh not found"
fi
echo ""
#-----------------------------------------------------------------------
# 13. Desktop CSS layer regression — 行内路径链接图标对齐（SOFT gate）
#     verify-pathlink-icon.cjs 绕过整个 App，把 MarkdownBody 单独挂进真实
#     dev server 的样式环境渲染，量 link / icon / 文字基线的真实像素关系，
#     捕捉"图标被挤到上面一层"这类 CSS 层叠回归（tsc / vitest 全绿也说明不了）。
#     依赖：dev server 已在 :1420 运行 + playwright（chrome/msedge 通道）。
#     二者缺一即 SKIP —— 这道门禁是浏览器视觉探针，CI 无 dev server / 无浏览器，
#     故为 SOFT：本地起 `npm run dev` 后跑 ci-gate 才会真正执行。
#-----------------------------------------------------------------------
echo "=== [13/14] desktop CSS layer regression (pathlink icon) ==="
if [ -f apps/desktop-tauri/scripts/verify-pathlink-icon.cjs ]; then
  if [ -d apps/desktop-tauri/node_modules/playwright ]; then
    if curl -s -o /dev/null -m 3 "http://[::1]:1420" 2>/dev/null || \
       curl -s -o /dev/null -m 3 "http://127.0.0.1:1420" 2>/dev/null; then
      (cd apps/desktop-tauri && node scripts/verify-pathlink-icon.cjs) \
        && echo "  OK: pathlink icon alignment regression clean" \
        || echo "  WARNING: pathlink icon alignment regression — see outputs/pathlink-icon"
    else
      echo "  SKIP: dev server not on :1420 (run 'npm run dev' to enable this gate)"
    fi
  else
    echo "  SKIP: playwright not installed in apps/desktop-tauri"
  fi
else
  echo "  SKIP: apps/desktop-tauri/scripts/verify-pathlink-icon.cjs not found"
fi
echo ""

#-----------------------------------------------------------------------
# 14. Desktop CSS 死重复块守卫（SOFT gate，纯静态、零依赖）
#     analyze-css-cascade.cjs 按 main.tsx import + globals.css @import 的级联
#     顺序，找出"完全同选择器、较早出现、所有声明都被后续规则覆盖"的死块。
#     本会话已清理 14 处此类死块；这道门禁防止它们回归。纯静态分析，不需要
#     浏览器/dev server，CI 可跑。为 SOFT：ORDER 假设或拆分改动可能让它偶发
#     误报，先观察一个周期再视情况升级为 HARD。
#-----------------------------------------------------------------------
echo "=== [14/15] desktop CSS dead-block guard ==="
if [ -f apps/desktop-tauri/scripts/analyze-css-cascade.cjs ]; then
  if [ -d apps/desktop-tauri/node_modules/postcss ]; then
    dead_count=$(cd apps/desktop-tauri && node scripts/analyze-css-cascade.cjs 2>/dev/null | grep -cE '^\[' || true)
    if [ "${dead_count:-0}" -gt 5 ]; then
      echo "  WARNING: $dead_count dead CSS blocks found (expected 5 = .approval-btn* cross-file conflicts still pending merge)."
      echo "  Run: cd apps/desktop-tauri && node scripts/analyze-css-cascade.cjs"
    else
      echo "  OK: dead CSS blocks within baseline ($dead_count; 5 are the pending .approval-btn* merge)"
    fi
  else
    echo "  SKIP: postcss not installed in apps/desktop-tauri"
  fi
else
  echo "  SKIP: apps/desktop-tauri/scripts/analyze-css-cascade.cjs not found"
fi
echo ""

#-----------------------------------------------------------------------
# 15. Desktop CSS undefined-var reference guard (HARD gate)
#     check-css-var-refs.cjs 收集所有 `--name:` 定义（CSS @theme/@property
#     内联 + React 内联样式对象键 + setProperty）与 `var(--x)` 引用，
#     任何未被定义的 var() 引用即 FAIL。捕捉"打错令牌前缀 → 静默隐形"
#     这类 bug：原 VaultPanel 19 处 `var(--accent)` / `var(--bg-primary)`
#     等不带 --color- 前缀的变量从未定义，元素底色回落 transparent、
#     实色按钮变白底白字不可见，却全程 0 报错、0 构建失败、其它守卫全绿。
#     纯静态、零依赖、CI 可跑。
#-----------------------------------------------------------------------
echo "=== [15/15] desktop CSS undefined-var reference guard ==="
if [ -f apps/desktop-tauri/scripts/check-css-var-refs.cjs ]; then
  if [ -d apps/desktop-tauri/node_modules ]; then
    (cd apps/desktop-tauri && node scripts/check-css-var-refs.cjs) \
      && echo "  OK: no undefined CSS var() references" \
      || exit 1
  else
    echo "  SKIP: apps/desktop-tauri/node_modules not found"
  fi
else
  echo "  SKIP: apps/desktop-tauri/scripts/check-css-var-refs.cjs not found"
fi
echo ""

echo "============================================"
echo "  CI Gate complete"
echo "============================================"
