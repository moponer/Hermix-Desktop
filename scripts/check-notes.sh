#!/usr/bin/env bash
# Decision Notes 结构门禁（抄 dsh agent-note-tree 思路，bash 裁剪版）
# 检查 docs/notes/ 的生命周期目录、文件命名、头三行格式与强制节。
# 由 scripts/ci-gate.sh 调用；违规即非零退出。
set -uo pipefail

NOTES_DIR="$(git rev-parse --show-toplevel 2>/dev/null)/docs/notes"
[ -d "$NOTES_DIR" ] || { echo "notes: docs/notes/ 不存在"; exit 0; }

LIFECYCLES="proposed implemented rejected"
CLASSES="feature bug-fix simplification architecture process testing"
errors=0
fail() { echo "notes: $1"; errors=$((errors+1)); }
warn() { echo "notes: WARN: $1"; }

# 1) 生命周期目录闭集：未知子目录即违规
for entry in "$NOTES_DIR"/*/; do
  name=$(basename "$entry")
  [ "$name" = "archived" ] && continue
  ok=0
  for l in $LIFECYCLES; do [ "$name" = "$l" ] && ok=1; done
  [ $ok -eq 0 ] && fail "$name/ — 未知 lifecycle（允许: $LIFECYCLES, archived/）"
done

# 2) 逐篇检查：路径 = {lifecycle}/{class}/yyyy-mm-dd-topic.md
shopt -s nullglob
for f in "$NOTES_DIR"/{proposed,implemented,rejected}/*/*.md; do
  rel=${f#"$NOTES_DIR"/}
  cls=$(basename "$(dirname "$f")")
  base=$(basename "$f" .md)

  ok=0
  for c in $CLASSES; do [ "$cls" = "$c" ] && ok=1; done
  [ $ok -eq 0 ] && fail "$rel — 未知 class（允许: $CLASSES）"

  echo "$base" | grep -Eq '^[0-9]{4}-[0-9]{2}-[0-9]{2}-[a-z0-9-]+$' \
    || fail "$rel — 文件名须为 yyyy-mm-dd-topic（小写连字符）"

  # 头三行：# Decision Note: 标题 / 空行 / Status: ...
  head -3 "$f" | grep -q '^# Decision Note: ' || fail "$rel — 第 1 行须为 '# Decision Note: <标题>'"
  head -3 "$f" | grep -qE '^Status: (proposed|implemented|rejected) — ' \
    || fail "$rel — 第 3 行须为 'Status: proposed|implemented|rejected — 现状'"

  # 强制节
  grep -q '^## Decision' "$f"          || fail "$rel — 缺 '## Decision' 节"
  grep -q '^## Alternatives considered' "$f" || fail "$rel — 缺 '## Alternatives considered' 节"
  grep -q '^## Consequences' "$f"      || fail "$rel — 缺 '## Consequences' 节"
  # simplification 必须写复职条件
  if [ "$cls" = "simplification" ]; then
    grep -q '^## Reintroduction conditions' "$f" \
      || fail "$rel — simplification 类缺 '## Reintroduction conditions' 节"
  fi
done

# 3) archived 件必须带 Archived: 行（在 Status 行之后 5 行内）
for f in "$NOTES_DIR"/archived/*/*.md; do
  grep -q '^Archived: [0-9]\{4\}-[0-9]\{2\}-[0-9]\{2\}$' "$f" \
    || fail "archived/${f##*/notes/} — 缺 'Archived: YYYY-MM-DD' 行"
done

# 4) 禁止生成式索引
[ -f "$NOTES_DIR/INDEX.md" ] && fail "INDEX.md — 生成式索引被禁止；目录树即清单"

# 5) docs/ 顶层新增 .md 守卫：决策/规划必须走 docs/notes/ 三态，
#    不得再往 docs/ 顶层丢 XXX_PLAN.md（AGENTS.md 文档制度节）。
#    保留白名单 = 迁移表"保留"行的现行文档 + 本守卫需要的制度文件。
#    批次 A/B 文档清理已完成（2026-10-01），此处已由 warn 升级为 fail：
#    `docs/` 顶层现在只剩白名单里的现行文档，任何新 .md 都是制度违规。
DOCS_DIR="$(dirname "$NOTES_DIR")"
ALLOWED="HANDOFF_FOUNDATION_REWRITE.md|HERMIX_AGENT_SYSTEM_PROMPT.md|TYPOGRAPHY_SYSTEM.md|UI_PROPOSAL_CC_HAHA_2026-09-29.md|UI_REDESIGN_PAPER_INK_2026-09-28.md|DEEPSEEK_HARNESS_BORROWINGS.md"
for f in "$DOCS_DIR"/*.md; do
  [ -e "$f" ] || continue
  base=$(basename "$f")
  echo "$base" | grep -qE "^($ALLOWED)$" || \
    fail "docs/$base — docs/ 顶层禁止新增 .md；决策/规划写入 docs/notes/{proposed,implemented}/（制度见 docs/notes/README.md）"
done

if [ $errors -gt 0 ]; then
  echo "notes: $errors 处违规"
  exit 1
fi
echo "notes: OK"
