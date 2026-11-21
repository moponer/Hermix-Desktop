# 用构建产物直接升级「便携式」安装目录（绕过 NSIS 安装器）
#
# 为什么需要它
#   Hermix Desktop 是便携式应用（数据就在 exe 旁边）。安装器的唯一作用就是
#   把几个 exe + loader 资源拷进安装目录。当 NSIS 安装器在这台机器上被
#   安全软件（360 / 腾讯管家）或「程序正在运行」挡住、报「无法写入」时，
#   直接拷贝是等价且更可靠的做法。
#
# 做什么
#   1. 关掉正在运行的 Hermix 进程（否则 exe 被锁住拷不进去）
#   2. 备份安装目录里即将被覆盖的文件
#   3. 从构建产物拷入主程序 + 4 个 sidecar + loader/hx-shell.exe
#   4. 逐文件 SHA256 校验源与目标一致
#
# 用法
#   .\scripts\install-portable-update.ps1                        # 用默认安装目录
#   .\scripts\install-portable-update.ps1 -InstallDir "D:\qcc\Hermix Desktop"
#   .\scripts\install-portable-update.ps1 -NoKill                 # 不自动关进程（会失败则报错）

param(
    [string]$InstallDir = "D:\qcc\Hermix Desktop",
    [string]$RepoRoot   = (Split-Path $PSScriptRoot -Parent),
    [switch]$NoKill
)

$ErrorActionPreference = "Stop"

$binDir  = Join-Path $RepoRoot "apps\desktop-tauri\src-tauri\binaries"
$relDir  = Join-Path $RepoRoot "target\release"
$ldrDir  = Join-Path $RepoRoot "apps\desktop-tauri\src-tauri\resources\loader"

if (-not (Test-Path $InstallDir)) {
    Write-Host "安装目录不存在: $InstallDir" -ForegroundColor Red
    exit 1
}

# 源文件 -> 安装目录内相对路径
$pairs = @(
    @{ Src = Join-Path $relDir "hermix-desktop.exe";                          Dst = "hermix-desktop.exe" }
    @{ Src = Join-Path $binDir "hermix-x86_64-pc-windows-msvc.exe";           Dst = "hermix.exe" }
    @{ Src = Join-Path $binDir "hermix-viewer-x86_64-pc-windows-msvc.exe";    Dst = "hermix-viewer.exe" }
    @{ Src = Join-Path $binDir "hermix-filesearch-x86_64-pc-windows-msvc.exe";Dst = "hermix-filesearch.exe" }
    @{ Src = Join-Path $binDir "markitdown-x86_64-pc-windows-msvc.exe";       Dst = "markitdown.exe" }
    @{ Src = Join-Path $ldrDir "hx-shell.exe";                                Dst = "loader\hx-shell.exe" }
    @{ Src = Join-Path $ldrDir "hx-shell.exe";                                Dst = "resources\loader\hx-shell.exe" }
)

# 1. 检查源文件齐不齐（缺一个就整体中止，不要拷一半）
$missing = @()
foreach ($p in $pairs) {
    if (-not (Test-Path $p.Src)) { $missing += $p.Src }
}
if ($missing.Count -gt 0) {
    Write-Host "构建产物缺失，先跑 npm run build：" -ForegroundColor Red
    $missing | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
    exit 1
}

# 2. 关掉正在运行的进程
$procs = Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.ProcessName -like "hermix*" }
if ($procs) {
    if ($NoKill) {
        Write-Host "进程仍在运行且指定了 -NoKill，无法覆盖：" -ForegroundColor Red
        $procs | ForEach-Object { Write-Host "  $($_.ProcessName) pid=$($_.Id)" -ForegroundColor Red }
        exit 1
    }
    Write-Host "关闭运行中的进程…" -ForegroundColor Yellow
    $procs | ForEach-Object {
        Write-Host "  $($_.ProcessName) pid=$($_.Id)"
        Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Seconds 2
}

# 3. 备份 + 拷贝
$stamp  = Get-Date -Format "yyyyMMdd-HHmmss"
$backup = Join-Path (Split-Path $InstallDir -Parent) ("_hermix_backup_" + $stamp)
New-Item -ItemType Directory -Force -Path $backup | Out-Null
Write-Host "备份目录: $backup" -ForegroundColor Cyan

foreach ($p in $pairs) {
    $dstPath = Join-Path $InstallDir $p.Dst
    $parent  = Split-Path $dstPath -Parent
    if (-not (Test-Path $parent)) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }

    if (Test-Path $dstPath) {
        $flat = $p.Dst -replace '[\\/]', '_'
        Copy-Item $dstPath (Join-Path $backup $flat) -Force
    }
    Copy-Item $p.Src $dstPath -Force
    Write-Host "  已更新 $($p.Dst)" -ForegroundColor Green
}

# 4. 校验
Write-Host ""
Write-Host "SHA256 校验:" -ForegroundColor Cyan
$bad = 0
foreach ($p in $pairs) {
    $dstPath = Join-Path $InstallDir $p.Dst
    $h1 = (Get-FileHash $p.Src  -Algorithm SHA256).Hash
    $h2 = (Get-FileHash $dstPath -Algorithm SHA256).Hash
    if ($h1 -eq $h2) {
        Write-Host "  OK   $($p.Dst)" -ForegroundColor Green
    } else {
        Write-Host "  MISMATCH $($p.Dst)" -ForegroundColor Red
        $bad++
    }
}

Write-Host ""
if ($bad -gt 0) {
    Write-Host "$bad 个文件校验失败。" -ForegroundColor Red
    exit 1
}

$ver = (Get-Item (Join-Path $InstallDir "hermix-desktop.exe")).VersionInfo.FileVersion
Write-Host "升级完成，当前版本 $ver。启动：" -ForegroundColor Green
Write-Host "  $InstallDir\hermix-desktop.exe"
Write-Host ""
Write-Host "回滚：把 $backup 里的文件拷回安装目录。" -ForegroundColor DarkGray
