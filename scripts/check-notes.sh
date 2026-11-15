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

# 3) archived 件必须带 Archived: 行（纯日期独占一行，原因写在下一行）
#    `$f` 已含 notes/ 前缀，不要再拼 "archived/" —— 会输出
#    "archived/archived/architecture/…" 这种看似路径错误的假象。
for f in "$NOTES_DIR"/archived/*/*.md; do
  grep -q '^Archived: [0-9]\{4\}-[0-9]\{2\}-[0-9]\{2\}$' "$f" \
    || fail "${f##*/notes/} — 缺 'Archived: YYYY-MM-DD' 行（须独占一行）"
done

# 4) 禁止生成式索引
[ -f "$NOTES_DIR/INDEX.md" ] && fail "INDEX.md — 生成式索引被禁止；目录树即清单"

# 5) docs/ 顶层新增 .md 守卫：决策/规划必须走 docs/notes/ 三态，
#    不得再往 docs/ 顶层丢 XXX_PLAN.md（AGENTS.md 文档制度节）。
#    2026-11-15 文档清理后 docs/ 顶层 .md 清零（过期已删、历史分析归
#    notes/archived/），白名单为空 —— 任何顶层新 .md 都是制度违规。
DOCS_DIR="$(dirname "$NOTES_DIR")"
ALLOWED=""
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
