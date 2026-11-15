# notes 结构门禁的 PowerShell 等价实现（本机没有 bash，无法直接跑
# scripts/check-notes.sh）。规则逐条照抄 bash 版，判定必须一致。
#
# 用法：pwsh -NoProfile -File scripts/check-notes.ps1
$ErrorActionPreference = 'Stop'
$repo = (git rev-parse --show-toplevel).Trim()
$notesDir = Join-Path $repo 'docs/notes'
if (-not (Test-Path $notesDir)) { Write-Output 'notes: docs/notes/ 不存在'; exit 0 }

$lifecycles = @('proposed', 'implemented', 'rejected')
$classes = @('feature', 'bug-fix', 'simplification', 'architecture', 'process', 'testing')
$errors = 0
function Fail([string]$msg) { Write-Output "notes: $msg"; $script:errors++ }

# 1) 生命周期目录闭集
Get-ChildItem $notesDir -Directory | ForEach-Object {
  if ($_.Name -eq 'archived') { return }
  if ($lifecycles -notcontains $_.Name) { Fail "$($_.Name)/ — 未知 lifecycle" }
}

# 2) 逐篇检查
foreach ($lc in $lifecycles) {
  $lcDir = Join-Path $notesDir $lc
  if (-not (Test-Path $lcDir)) { continue }
  foreach ($clsDir in Get-ChildItem $lcDir -Directory) {
    $cls = $clsDir.Name
    if ($classes -notcontains $cls) { Fail "$lc/$cls — 未知 class" }
    foreach ($f in Get-ChildItem $clsDir.FullName -Filter '*.md' -File) {
      $rel = "$lc/$cls/$($f.Name)"
      $base = $f.BaseName
      if ($base -notmatch '^\d{4}-\d{2}-\d{2}-[a-z0-9-]+$') { Fail "$rel — 文件名须为 yyyy-mm-dd-topic" }
      $head = (Get-Content $f.FullName -TotalCount 3) -replace "
", "
"
      if (-not ($head -match '^# Decision Note: ')) { Fail "$rel — 第 1 行须为 '# Decision Note: <标题>'" }
      if (-not ($head -match '^Status: (proposed|implemented|rejected) — ')) { Fail "$rel — 第 3 行须为 'Status: ...'" }
      $all = (Get-Content $f.FullName -Raw) -replace "`r`n", "`n"
      if ($all -notmatch '(?m)^## Decision') { Fail "$rel — 缺 '## Decision' 节" }
      if ($all -notmatch '(?m)^## Alternatives considered') { Fail "$rel — 缺 '## Alternatives considered' 节" }
      if ($all -notmatch '(?m)^## Consequences') { Fail "$rel — 缺 '## Consequences' 节" }
      if ($cls -eq 'simplification' -and $all -notmatch '(?m)^## Reintroduction conditions') {
        Fail "$rel — simplification 类缺 '## Reintroduction conditions' 节"
      }
    }
  }
}

# 3) archived 件必须带 Archived: 行
$arch = Join-Path $notesDir 'archived'
if (Test-Path $arch) {
  Get-ChildItem $arch -Recurse -Filter '*.md' -File | ForEach-Object {
    $all = (Get-Content $_.FullName -Raw) -replace "`r`n", "`n"
    if ($all -notmatch '(?m)^Archived: \d{4}-\d{2}-\d{2}$') { Fail "archived/$($_.Name) — 缺 'Archived: YYYY-MM-DD' 行" }
  }
}

# 4) 禁止生成式索引
if (Test-Path (Join-Path $notesDir 'INDEX.md')) { Fail 'INDEX.md — 生成式索引被禁止' }

# 5) docs/ 顶层 .md 白名单（2026-11-15 文档清理后清零：过期已删、历史分析归 notes/archived/）
$allowed = @()
Get-ChildItem (Join-Path $repo 'docs') -Filter '*.md' -File | ForEach-Object {
  if ($allowed -notcontains $_.Name) { Fail "docs/$($_.Name) — docs/ 顶层禁止新增 .md" }
}

if ($errors -gt 0) { Write-Output "notes: $errors 处违规"; exit 1 }
Write-Output 'notes: OK'
