#!/usr/bin/env bash
# Repository hygiene gate (batch J3 of the 2026-09-30 plan).
#
# Complements check-notes.sh, which owns the notes TREE (structure,
# lifecycle dirs, mandatory sections). This one owns the places agents
# actually pollute: the repo root and the top-level directory list.
#
# What it catches:
#   1. Stray files in the repo root (*.log / *.txt / *.py / *.tgz …)
#      that are not on the allowlist. Agents drop probe scripts and
#      route dumps there; a later session reads them as project assets.
#   2. New top-level directories. A new crate or a new top-level tree
#      is an architecture decision, not something to appear as a side
#      effect of a work session.
#
# Scoped to TRACKED files on purpose: untracked local scratch is the
# developer's business, and a dirty worktree must not fail the gate.
# A new junk file has to be `git add`ed to become a problem — and that
# is exactly the moment worth failing on.
#
# Called from scripts/ci-gate.sh as a hard gate.
set -uo pipefail

ROOT="$(git rev-parse --show-toplevel 2>/dev/null)"
if [ -z "$ROOT" ]; then
  echo "hygiene: not a git repository; skipping"
  exit 0
fi
cd "$ROOT"

errors=0
fail() { echo "hygiene: $1"; errors=$((errors + 1)); }

# ---------------------------------------------------------------------------
# 1) Repo-root stray files
#
# Scope note: `.gitignore` already covers the classic debris — `*.log`
# globally, and `/*.txt`, `/*.py`, `/*.bat` at the root. Those can never
# reach the index, so checking them here would be theatre. What is left
# is the set that IS trackable and IS what actually turns up in review:
# archives, dumps, and typeable config/data dropped at the root.
#
# Tracked-only, on purpose: a dirty worktree must not fail the gate, and
# untracked local scratch is the developer's business. A junk file has
# to be `git add`ed to become a repo problem — which is exactly the
# moment worth failing on.
# ---------------------------------------------------------------------------
ROOT_ALLOWED="^(README\.md|AGENTS\.md|CLAUDE\.md|SOUL\.md|Cargo\.toml|Cargo\.lock|rust-toolchain.*|rustfmt\.toml|clippy\.toml|package\.json|pnpm-lock\.yaml|.*\.code-workspace|LICENSE.*|NOTICE.*|THIRD_PARTY_NOTICES.*)$"

# `git ls-files ':/*'` does NOT mean "top level" — `*` is not
# slash-bound, so it returns every tracked path. Filter on "no slash".
STRAY=$(git ls-files 2>/dev/null \
        | grep -v '/' \
        | grep -E '\.(tgz|tar\.gz|zip|7z|rar|dmp|core|md5|sha1)$' || true)
for f in $STRAY; do
  base=$(basename "$f")
  echo "$base" | grep -qE "$ROOT_ALLOWED" && continue
  fail "$f — 仓库根不允许散件。一次性脚本请用 code_execution 内存跑；\
确需落盘的临时文件放 %TEMP%\\hermix-scratch\\；方案/规划写 docs/notes/proposed/{class}/\
（见 AGENTS.md 文档制度节）"
done

# ---------------------------------------------------------------------------
# 2) New top-level directories
#    A directory that shows up at the root is either a build artifact
#    (allowlisted) or an unreviewed architecture change.
# ---------------------------------------------------------------------------
# `sdk/` and `skills/` are pre-existing tracked trees (protocol
# fixtures + TypeScript SDK, and the skill packages) — they are part of
# the baseline, not something a work session introduced.
# 2026-10-21：`packages/` 同理 —— 它是 monorepo 的既定结构（`@hermix/preview`
# 等内部包），与 `crates/`（Rust crate）、`apps/`（应用）同级，是 2026-10-13
# 起就在的基线树，不是某次工作引入的「新建顶层目录」。漏掉它会让这条门禁
# 在干净仓库上恒红，久而久之大家就学会忽略它的输出。
# `branding/`（品牌素材）、`patches/`（第三方补丁）、`hermix-logo-concepts_assets/`
# （logo 概念图）都是 HEAD 里已跟踪的既有目录（2026-10-23 补进白名单）——
# 白名单落后于仓库现状时，门禁会一直红，而"一直红"的门禁等于没有门禁；
# 新增顶层目录仍会被这条检查抓住。
TOP_ALLOWED="^(crates|apps|packages|docs|scripts|sdk|skills|assets|release|target|node_modules|\.git|\.github|\.vscode|\.idea|\.worktrees|third_party|branding|patches|hermix-logo-concepts_assets)$"
for d in */; do
  [ -e "$d" ] || continue
  name=$(basename "$d")
  echo "$name" | grep -qE "$TOP_ALLOWED" && continue
  # Only complain about directories that are actually tracked — an
  # untracked local experiment folder is not a repo-structure change.
  git ls-files --error-unmatch "$d" >/dev/null 2>&1 || continue
  fail "$name/ — 新建顶层目录属于架构决策，先写 docs/notes/proposed/architecture/ 决策笔记"
done

if [ $errors -gt 0 ]; then
  echo "hygiene: $errors 处违规"
  exit 1
fi
echo "hygiene: OK"
